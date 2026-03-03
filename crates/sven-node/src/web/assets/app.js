/**
 * Sven Node — Web Terminal Frontend
 *
 * State machine:
 *
 *   INIT ──► REGISTER ──► PENDING ──► LOGIN ──► TERMINAL
 *                │                       │
 *                └──────── (return visit) ┘
 *
 * INIT:      Check localStorage for stored device_id.
 *            If none → REGISTER. Else → LOGIN.
 *
 * REGISTER:  WebAuthn passkey registration ceremony.
 *            On success → store device_id → PENDING.
 *
 * PENDING:   Show "awaiting admin approval" with device ID.
 *            SSE stream fires when admin approves → LOGIN.
 *
 * LOGIN:     WebAuthn passkey authentication ceremony.
 *            On success (session cookie set) → TERMINAL.
 *            On 403 pending → PENDING.
 *
 * TERMINAL:  xterm.js mounted; PTY WebSocket opened.
 *            Resize events propagated. Session persists across reconnects.
 */

'use strict';

// ── Storage keys ──────────────────────────────────────────────────────────────
const STORAGE_DEVICE_ID = 'sven_device_id';

// ── DOM helpers ───────────────────────────────────────────────────────────────
const $ = id => document.getElementById(id);
const cardBody = () => $('card-body');
const errMsg   = () => $('error-msg');
const setError = msg => { errMsg().textContent = msg || ''; };
const clearError = () => setError('');

// ── State machine ─────────────────────────────────────────────────────────────
let state = 'INIT';

async function transition(next, data) {
  clearError();
  state = next;
  switch (next) {
    case 'INIT':     return runInit();
    case 'REGISTER': return renderRegister();
    case 'PENDING':  return renderPending(data);
    case 'LOGIN':    return renderLogin();
    case 'TERMINAL': return startTerminal();
    case 'REVOKED':  return renderRevoked();
  }
}

// ── INIT ──────────────────────────────────────────────────────────────────────
async function runInit() {
  // Fetch the server's canonical rp_origin.  If the browser is on a different
  // origin (e.g. https://localhost vs https://myhost.ts.net) WebAuthn will
  // silently fail.  Detect this early and redirect automatically so the user
  // never sees an opaque "NotAllowedError".
  try {
    const infoRes = await fetch('/web/auth/info');
    if (infoRes.ok) {
      const { rp_origin } = await infoRes.json();
      const serverOrigin = rp_origin.replace(/\/$/, '');
      const myOrigin     = window.location.origin.replace(/\/$/, '');
      if (serverOrigin && serverOrigin !== myOrigin) {
        // Redirect, preserving path and query string.
        const dest = serverOrigin + window.location.pathname + window.location.search;
        cardBody().innerHTML = `
          <h2>Redirecting…</h2>
          <p style="font-size:0.875rem;color:#888;">
            This node is served at <strong>${serverOrigin}</strong>.<br>
            Redirecting you there now so WebAuthn works correctly.
          </p>
          <p style="font-size:0.8rem;color:#555;">
            If you are not redirected automatically,
            <a href="${dest}" style="color:#5b8dee;">click here</a>.
          </p>`;
        setTimeout(() => { window.location.replace(dest); }, 1500);
        return;
      }
    }
  } catch (_) {
    // Ignore fetch errors — fall through to normal REGISTER/LOGIN flow.
  }

  const deviceId = localStorage.getItem(STORAGE_DEVICE_ID);
  if (deviceId) {
    transition('LOGIN');
  } else {
    transition('REGISTER');
  }
}

// ── REGISTER ──────────────────────────────────────────────────────────────────
function renderRegister() {
  cardBody().innerHTML = `
    <h2>Register this device</h2>
    <p>Use your device biometrics (Face ID, fingerprint, or security key) to create a passkey. An admin must approve the device before you can access the terminal.</p>
    <button class="btn btn-primary" id="btn-register">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg>
      Register with passkey
    </button>
  `;
  $('btn-register').addEventListener('click', doRegister);
}

async function doRegister() {
  const btn = $('btn-register');
  btn.disabled = true;
  btn.textContent = 'Registering…';
  clearError();

  try {
    // Step 1: Get challenge.
    const challengeRes = await fetch('/web/auth/register/challenge', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ display_name: navigator.userAgent.slice(0, 60) }),
    });
    if (!challengeRes.ok) throw new Error('Failed to get registration challenge');
    const { challenge_id, public_key } = await challengeRes.json();

    // Convert base64url buffers that webauthn-rs sends.
    coerceCreationOptions(public_key);

    // Step 2: Browser ceremony (biometric prompt).
    const credential = await navigator.credentials.create({ publicKey: public_key.publicKey });
    if (!credential) throw new Error('Passkey creation was cancelled');

    // Step 3: Send attestation to server.
    const completeRes = await fetch('/web/auth/register/complete', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        challenge_id,
        credential: encodeCredential(credential),
        display_name: null,
      }),
    });
    if (!completeRes.ok) {
      const msg = await completeRes.text();
      throw new Error(msg || 'Registration verification failed');
    }
    const { device_id } = await completeRes.json();
    localStorage.setItem(STORAGE_DEVICE_ID, device_id);
    transition('PENDING', device_id);

  } catch (e) {
    btn.disabled = false;
    btn.textContent = 'Register with passkey';

    if (e.name === 'NotAllowedError') {
      // Re-check the canonical origin in case the redirect in INIT was missed
      // (e.g. /web/auth/info was temporarily unreachable on page load).
      // If we're on the wrong origin, redirect now rather than leaving the user
      // stuck with a cryptic "not allowed" message.
      try {
        const infoRes = await fetch('/web/auth/info');
        if (infoRes.ok) {
          const { rp_origin } = await infoRes.json();
          const serverOrigin = rp_origin.replace(/\/$/, '');
          const myOrigin     = window.location.origin.replace(/\/$/, '');
          if (serverOrigin && serverOrigin !== myOrigin) {
            const dest = serverOrigin + window.location.pathname + window.location.search;
            setError(
              `Passkey creation failed because this page must be accessed via ` +
              `${serverOrigin}. Redirecting…`
            );
            setTimeout(() => window.location.replace(dest), 2000);
            return;
          }
        }
      } catch (_) { /* ignore — fall through to generic hint */ }

      setError(
        'Passkey creation was blocked by the browser. ' +
        'This usually means the server\'s TLS certificate is not trusted by ' +
        'your device\'s platform authenticator, or the configured passkey ' +
        'domain does not match the URL you are using. ' +
        'If you are accessing via a custom hostname or Tailscale address, ' +
        'ensure the certificate is trusted at the OS level (not just the browser).'
      );
    } else {
      setError(e.message);
    }
  }
}

// ── PENDING ───────────────────────────────────────────────────────────────────
function renderPending(deviceId) {
  const id = deviceId || localStorage.getItem(STORAGE_DEVICE_ID) || '?';
  cardBody().innerHTML = `
    <h2><span class="status-dot"></span>Awaiting approval</h2>
    <p>Show this device ID to your node admin and ask them to run:</p>
    <div class="device-id-box">${id}</div>
    <p style="margin-bottom:0; font-size:0.8rem; color:#555;">
      <code>sven node web-devices approve ${id.slice(0, 8)}</code>
    </p>
  `;

  // Subscribe to approval SSE.
  const evtSource = new EventSource(`/web/auth/status?device=${encodeURIComponent(id)}`);
  evtSource.addEventListener('approved', () => {
    evtSource.close();
    transition('LOGIN');
  });
  evtSource.addEventListener('revoked', () => {
    evtSource.close();
    localStorage.removeItem(STORAGE_DEVICE_ID);
    transition('REVOKED');
  });
  evtSource.onerror = () => {
    // SSE connection dropped — retry silently (browser auto-reconnects).
  };
}

// ── LOGIN ─────────────────────────────────────────────────────────────────────
function renderLogin() {
  const deviceId = localStorage.getItem(STORAGE_DEVICE_ID);
  if (!deviceId) { transition('REGISTER'); return; }

  cardBody().innerHTML = `
    <h2>Welcome back</h2>
    <p>Authenticate with your passkey to open the terminal.</p>
    <button class="btn btn-primary" id="btn-login">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/></svg>
      Authenticate with passkey
    </button>
    <button class="btn btn-secondary" id="btn-forget">Use a different device</button>
  `;
  $('btn-login').addEventListener('click', doLogin);
  $('btn-forget').addEventListener('click', () => {
    localStorage.removeItem(STORAGE_DEVICE_ID);
    transition('REGISTER');
  });
}

async function doLogin() {
  const deviceId = localStorage.getItem(STORAGE_DEVICE_ID);
  const btn = $('btn-login');
  btn.disabled = true;
  btn.textContent = 'Authenticating…';
  clearError();

  try {
    // Step 1: Get auth challenge.
    const challengeRes = await fetch('/web/auth/login/challenge', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ device_id: deviceId }),
    });
    if (challengeRes.status === 403) {
      const msg = await challengeRes.text();
      if (msg.includes('pending')) { transition('PENDING', deviceId); return; }
      if (msg.includes('revoked')) { localStorage.removeItem(STORAGE_DEVICE_ID); transition('REVOKED'); return; }
      throw new Error(msg);
    }
    if (challengeRes.status === 404) {
      // Device record was deleted on the server (e.g. the node was reconfigured
      // with a new rp_id and all credentials were purged automatically).
      // Clear the stale local ID and offer re-registration.
      localStorage.removeItem(STORAGE_DEVICE_ID);
      btn.disabled = false;
      btn.textContent = 'Authenticate with passkey';
      cardBody().innerHTML = `
        <h2>Device not found</h2>
        <p style="font-size:0.875rem;color:#888;">
          This device is no longer registered on the node. This can happen
          when the server address changes or the node is reconfigured.
        </p>
        <button class="btn btn-primary" id="btn-reregister-404">
          Register this device
        </button>`;
      $('btn-reregister-404').addEventListener('click', () => transition('REGISTER'));
      return;
    }
    if (!challengeRes.ok) throw new Error('Failed to get auth challenge');
    const { challenge_id, public_key } = await challengeRes.json();

    coerceRequestOptions(public_key);

    // Step 2: Browser ceremony.
    const assertion = await navigator.credentials.get({ publicKey: public_key.publicKey });
    if (!assertion) throw new Error('Authentication was cancelled');

    // Step 3: Verify with server (sets session cookie).
    const completeRes = await fetch('/web/auth/login/complete', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        challenge_id,
        credential: encodeAssertion(assertion),
      }),
    });
    if (completeRes.status === 403) {
      const msg = await completeRes.text();
      if (msg.includes('pending')) { transition('PENDING', deviceId); return; }
      localStorage.removeItem(STORAGE_DEVICE_ID);
      transition('REVOKED');
      return;
    }
    if (!completeRes.ok) throw new Error('Authentication failed');

    transition('TERMINAL');

  } catch (e) {
    btn.disabled = false;
    btn.textContent = 'Authenticate with passkey';

    // NotAllowedError is the browser's opaque way of saying either:
    //   • no matching passkey found for the current origin/rp_id, OR
    //   • the stored passkey was registered against a different rp_id
    //     (e.g. the server was reconfigured from localhost to a domain), OR
    //   • the user cancelled / the operation timed out.
    //
    // Offer a re-registration escape hatch so the user never needs to
    // manually delete the device registry.
    //
    // Use appendChild instead of innerHTML+= to avoid destroying the existing
    // DOM elements and their event listeners.
    if (e.name === 'NotAllowedError') {
      const hint = document.createElement('div');
      hint.className = 'reregister-hint';
      hint.innerHTML = `
        <p style="font-size:0.8rem;color:#888;margin-top:16px;">
          Passkey not recognised — the server address or configuration may
          have changed since this device was registered.
        </p>
        <button class="btn btn-secondary" id="btn-reregister-hint">
          Register this device again
        </button>`;
      cardBody().appendChild(hint);
      $('btn-reregister-hint').addEventListener('click', () => {
        localStorage.removeItem(STORAGE_DEVICE_ID);
        transition('REGISTER');
      });
    } else {
      setError(e.message);
    }
  }
}

// ── REVOKED ───────────────────────────────────────────────────────────────────
function renderRevoked() {
  cardBody().innerHTML = `
    <h2>Device revoked</h2>
    <p>This device has been revoked by the node admin. Register a new device to regain access.</p>
    <button class="btn btn-primary" id="btn-reregister">Register new device</button>
  `;
  $('btn-reregister').addEventListener('click', () => {
    localStorage.removeItem(STORAGE_DEVICE_ID);
    transition('REGISTER');
  });
}

// ── TERMINAL ──────────────────────────────────────────────────────────────────
function startTerminal() {
  $('auth-screen').style.display = 'none';
  // Use flex so the status bar is a real sibling below the terminal,
  // not a fixed overlay that covers tmux's bottom status line.
  $('terminal-screen').style.display = 'flex';

  const termEl   = $('terminal');
  const statusBar = $('status-bar');
  const statusText = $('status-text');

  // ── xterm.js instance ─────────────────────────────────────────────────────
  const term = new Terminal({
    cursorBlink:  true,
    cursorStyle:  'block',
    fontFamily:   '"SF Mono", "Fira Code", "JetBrains Mono", "Consolas", monospace',
    // 11px gives ~59 cols on a 390 px phone in portrait (≈ 6.6 px/char).
    // 14px (old default) gave ~46.  True 80-col would need ~8 px but that is
    // too small to read comfortably; 11 px is the practical sweet spot.
    fontSize:     11,
    lineHeight:   1.2,
    // Allow the Meta key (Alt on Linux/Windows, Option on macOS) to pass
    // through as an escape prefix — required for many tmux/vim bindings.
    macOptionIsMeta: true,
    allowProposedApi: true,
    theme: {
      background:    '#0d0d0d',
      foreground:    '#e8e8e8',
      cursor:        '#5b8dee',
      cursorAccent:  '#0d0d0d',
      selection:     'rgba(91,141,238,0.3)',
      black:         '#1a1a1a', red:           '#f87171',
      green:         '#4ade80', yellow:        '#fbbf24',
      blue:          '#5b8dee', magenta:       '#c084fc',
      cyan:          '#67e8f9', white:         '#e8e8e8',
      brightBlack:   '#2d2d2d', brightRed:     '#fca5a5',
      brightGreen:   '#86efac', brightYellow:  '#fde68a',
      brightBlue:    '#93bbf5', brightMagenta: '#d8b4fe',
      brightCyan:    '#a5f3fc', brightWhite:   '#f8fafc',
    },
  });

  const fitAddon      = new FitAddon.FitAddon();
  const webLinksAddon = new WebLinksAddon.WebLinksAddon();
  term.loadAddon(fitAddon);
  term.loadAddon(webLinksAddon);
  term.open(termEl);
  // Defer the first fit until after the browser has done its layout pass.
  // Calling fitAddon.fit() synchronously after display:flex is set gives
  // xterm a zero-height container, which corrupts the initial cell grid and
  // produces scroll traces.  Two rAF calls guarantee we're past both the
  // style recalc and the paint frame.
  requestAnimationFrame(() => requestAnimationFrame(() => fitAddon.fit()));

  // ── Keyboard interception ─────────────────────────────────────────────────
  //
  // Browsers intercept many Ctrl+key combinations (bold, reload, new tab, …)
  // before xterm.js sees them.  tmux uses Ctrl+B as its prefix key, making
  // pane splits and window switching impossible without interception.
  //
  // Strategy: let xterm handle ALL key events except:
  //   Ctrl+Shift+C — browser clipboard copy  (does not conflict with tmux)
  //   Ctrl+Shift+V — browser clipboard paste (does not conflict with tmux)
  //
  // Note: Ctrl+W (close tab), Ctrl+T (new tab), Ctrl+N (new window) are
  // intercepted at the browser chrome level and CANNOT be overridden.
  // Use tmux bindings that don't conflict with those if needed.
  term.attachCustomKeyEventHandler(e => {
    // Allow Ctrl+Shift+C/V so the user can still copy/paste via the browser.
    if (e.ctrlKey && e.shiftKey) {
      if (e.code === 'KeyC' || e.code === 'KeyV') return false;
    }
    // Pass everything else to xterm → PTY.
    return true;
  });

  // ── WebSocket connection ───────────────────────────────────────────────────
  const wsProto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const wsUrl   = `${wsProto}//${location.host}/web/pty/ws`;

  let ws;
  let reconnectDelay    = 1000;
  const maxReconnectDelay = 30000;

  function connect() {
    ws = new WebSocket(wsUrl);
    ws.binaryType = 'arraybuffer';

    ws.onopen = () => {
      reconnectDelay = 1000;
      // Hide the status bar when cleanly connected — frees the bottom row
      // for tmux's own status line.
      statusBar.className = '';
      // Re-fit and report size so the PTY is in sync with the current
      // viewport.  The double-rAF ensures the status bar has collapsed
      // before we measure the container.
      requestAnimationFrame(() => requestAnimationFrame(() => {
        fitAddon.fit();
        sendResize();
        term.refresh(0, term.rows - 1);
      }));
    };

    ws.onmessage = evt => {
      const data = new Uint8Array(evt.data);
      if (data.length === 0) return;
      if (data[0] === 0x00) {
        term.write(data.slice(1));
      }
      // 0x01 control frames from server → reserved for future use.
    };

    ws.onclose = () => {
      statusBar.className = 'reconnecting';
      const secs = Math.round(reconnectDelay / 1000);
      statusText.textContent = `connection lost — reconnecting in ${secs}s…`;
      setTimeout(async () => {
        // Before reconnecting, verify the session is still valid.  When the
        // 24-hour JWT expires the WebSocket upgrade returns 401, which the
        // browser surfaces as a plain close event with no status code.
        // Detecting it here prevents an infinite reconnect loop and sends the
        // user back to the login screen instead.
        try {
          const res = await fetch('/web/auth/check');
          if (res.status === 401) {
            transition('LOGIN');
            return;
          }
        } catch (_) { /* network error — attempt reconnect anyway */ }
        statusText.textContent = 'reconnecting…';
        connect();
      }, reconnectDelay);
      reconnectDelay = Math.min(reconnectDelay * 2, maxReconnectDelay);
    };

    ws.onerror = () => {
      statusBar.className = 'disconnected';
      statusText.textContent = 'connection error';
    };
  }

  function sendResize() {
    if (ws && ws.readyState === WebSocket.OPEN) {
      const msg = JSON.stringify({ type: 'resize', cols: term.cols, rows: term.rows });
      const ctrl = new Uint8Array(1 + msg.length);
      ctrl[0] = 0x01;
      for (let i = 0; i < msg.length; i++) ctrl[i + 1] = msg.charCodeAt(i);
      ws.send(ctrl);
    }
  }

  // Forward keyboard input to PTY.
  term.onData(data => {
    if (ws && ws.readyState === WebSocket.OPEN) {
      const bytes = new TextEncoder().encode(data);
      ws.send(bytes);
    }
  });

  // Resize terminal whenever the containing element changes size.
  // After fitting, force a full terminal refresh so no stale canvas
  // regions linger from the previous geometry.
  const ro = new ResizeObserver(() => {
    fitAddon.fit();
    sendResize();
    term.refresh(0, term.rows - 1);
  });
  ro.observe(termEl);

  // ── Virtual keyboard avoidance (mobile) ───────────────────────────────────
  //
  // On mobile, when the on-screen keyboard opens the browser shrinks the
  // *visual* viewport but not always the layout viewport.  This means a
  // terminal sized to `height: 100%` or `100vh` can be half-hidden behind the
  // keyboard, with no way for the user to see what they're typing.
  //
  // The `visualViewport` API reports the *actually visible* rect in real-time.
  // We reposition `#terminal-screen` (which is position:fixed) to exactly fit
  // the visual viewport so the keyboard never overlaps the terminal.
  //
  // Two events fire:
  //   resize — keyboard opens/closes, browser chrome shows/hides
  //   scroll — iOS can scroll the visual viewport independently
  const termScreen = $('terminal-screen');

  function applyVisualViewport() {
    const vv = window.visualViewport;
    // Position and size the terminal screen to the visible area.
    termScreen.style.top    = Math.round(vv.offsetTop  + vv.pageTop)  + 'px';
    termScreen.style.left   = Math.round(vv.offsetLeft + vv.pageLeft) + 'px';
    termScreen.style.width  = Math.round(vv.width)  + 'px';
    termScreen.style.height = Math.round(vv.height) + 'px';
    // Re-fit after the geometry change; two rAFs ensure layout is done first.
    requestAnimationFrame(() => requestAnimationFrame(() => {
      fitAddon.fit();
      sendResize();
      term.refresh(0, term.rows - 1);
    }));
  }

  if (window.visualViewport) {
    window.visualViewport.addEventListener('resize', applyVisualViewport);
    window.visualViewport.addEventListener('scroll', applyVisualViewport);
    // Apply immediately so the initial size matches the visual viewport even
    // before any keyboard event fires (handles browser chrome like address bar).
    applyVisualViewport();
  }

  // ── Mobile touch handling ──────────────────────────────────────────────────
  //
  // Browsers do not synthesise WheelEvents or auto-focus from touch events,
  // so we handle two gestures explicitly:
  //
  //   tap   → term.focus() to open the virtual keyboard.
  //
  //   swipe → synthesise discrete WheelEvents (DOM_DELTA_LINE) on every
  //           PIXELS_PER_STEP pixels of movement so that:
  //             a) xterm's viewport handler scrolls the scrollback buffer, AND
  //             b) xterm's mouse-reporting handler (on .xterm-screen, a sibling
  //                of .xterm-viewport) forwards the event to the PTY/tmux.
  //           After the finger lifts, kinetic momentum scrolling decays the
  //           velocity so a quick flick continues scrolling naturally.
  //
  //   Horizontal-dominant swipes are ignored so tmux pane / window navigation
  //   (Ctrl+B arrow, next-window) is not accidentally converted to scroll.
  //
  //   touchstart is non-passive so that touchmove can call preventDefault(),
  //   which is required to stop the browser from performing its own panning
  //   gesture on top of our custom scroll.
  //
  // Architecture note on WheelEvent targets:
  //   .xterm-viewport  — owns the CSS overflow scroll; must receive the event
  //                       for xterm's built-in viewport-scroll handler.
  //   .xterm-screen    — owns the canvas; receives the event for xterm's mouse-
  //                       reporting handler (which sends escape seqs to the PTY).
  //   The two elements are siblings so we must dispatch to both explicitly.

  const xtermViewport = termEl.querySelector('.xterm-viewport') || termEl;
  const xtermScreen   = termEl.querySelector('.xterm-screen')   || termEl;

  // How many pixels of finger movement equal one discrete scroll step.
  // Lower = more sensitive; 24 px ≈ one comfortable scroll step.
  const SCROLL_STEP_PX = 24;

  function fireScrollStep(clientX, clientY, lines) {
    // lines > 0 → scroll DOWN (content moves up); lines < 0 → scroll UP.
    const opts = {
      deltaY:    lines,
      deltaMode: WheelEvent.DOM_DELTA_LINE,
      bubbles:   true,
      cancelable: true,
      view:      window,
      clientX,
      clientY,
    };
    xtermViewport.dispatchEvent(new WheelEvent('wheel', opts));
    xtermScreen.dispatchEvent(new WheelEvent('wheel', opts));
  }

  let touchStartX   = 0, touchStartY  = 0;
  let touchLastY    = 0, touchLastT   = 0;
  let touchVelocity = 0; // px/ms — positive = finger moving down = scroll up
  let touchDidScroll = false;
  let scrollAccum    = 0; // sub-step accumulator
  let momentumRafId  = null;

  function cancelMomentum() {
    if (momentumRafId !== null) { cancelAnimationFrame(momentumRafId); momentumRafId = null; }
  }

  termEl.addEventListener('touchstart', e => {
    cancelMomentum();
    touchStartX    = e.touches[0].clientX;
    touchStartY    = touchLastY = e.touches[0].clientY;
    touchLastT     = performance.now();
    touchVelocity  = 0;
    touchDidScroll = false;
    scrollAccum    = 0;
  }, { passive: false });

  termEl.addEventListener('touchmove', e => {
    const now     = performance.now();
    const dy      = e.touches[0].clientY - touchLastY; // positive = finger moved down
    const totalDy = Math.abs(e.touches[0].clientY - touchStartY);
    const totalDx = Math.abs(e.touches[0].clientX - touchStartX);

    // Gate: ignore horizontal-dominant gestures until the intent is clearly vertical.
    if (!touchDidScroll && totalDx > totalDy && totalDx > 8) return;

    e.preventDefault();
    touchDidScroll = true;

    // Exponential moving average for velocity (px/ms, signed like dy).
    const dt = Math.max(1, now - touchLastT);
    touchVelocity = touchVelocity * 0.6 + (dy / dt) * 0.4;

    // Accumulate movement; emit one scroll step per SCROLL_STEP_PX.
    // dy > 0 (finger down) → content scrolls UP → lines negative.
    scrollAccum += -dy;
    const steps = Math.trunc(scrollAccum / SCROLL_STEP_PX);
    if (steps !== 0) {
      scrollAccum -= steps * SCROLL_STEP_PX;
      fireScrollStep(e.touches[0].clientX, e.touches[0].clientY, steps);
    }

    touchLastY = e.touches[0].clientY;
    touchLastT = now;
  }, { passive: false });

  termEl.addEventListener('touchend', e => {
    if (!touchDidScroll) {
      term.focus(); // tap → open virtual keyboard
      return;
    }
    touchDidScroll = false;

    // Kinetic momentum: continue scrolling after the finger lifts.
    // Threshold: at least 0.15 px/ms to start momentum.
    const startVelocity = -touchVelocity; // flip: finger-down velocity → scroll-up lines
    if (Math.abs(startVelocity) < 0.15) return;

    let velocity   = startVelocity; // px/ms in scroll-direction
    let accumMom   = 0;
    let lastT      = performance.now();
    const cx       = e.changedTouches[0]?.clientX ?? touchStartX;
    const cy       = e.changedTouches[0]?.clientY ?? touchStartY;

    function step() {
      const now = performance.now();
      const dt  = now - lastT;
      lastT     = now;

      // Friction: ~97% velocity retained per ms → smooth decay over ~1–2 s.
      velocity *= Math.pow(0.97, dt);
      if (Math.abs(velocity) < 0.05) { momentumRafId = null; return; }

      accumMom += velocity * dt;
      const steps = Math.trunc(accumMom / SCROLL_STEP_PX);
      if (steps !== 0) {
        accumMom -= steps * SCROLL_STEP_PX;
        fireScrollStep(cx, cy, steps);
      }

      momentumRafId = requestAnimationFrame(step);
    }
    momentumRafId = requestAnimationFrame(step);
  });

  connect();
  term.focus();
}

// ── WebAuthn buffer coercion helpers ──────────────────────────────────────────
// webauthn-rs sends base64url-encoded strings; the browser API needs ArrayBuffers.

function b64url(str) {
  const b64 = str.replace(/-/g, '+').replace(/_/g, '/');
  const bin = atob(b64);
  const buf = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) buf[i] = bin.charCodeAt(i);
  return buf.buffer;
}

function coerceCreationOptions(opts) {
  const pk = opts.publicKey;
  if (pk.challenge)           pk.challenge           = b64url(pk.challenge);
  if (pk.user?.id)            pk.user.id             = b64url(pk.user.id);
  if (pk.excludeCredentials)  pk.excludeCredentials  = pk.excludeCredentials.map(c => ({ ...c, id: b64url(c.id) }));
}

function coerceRequestOptions(opts) {
  const pk = opts.publicKey;
  if (pk.challenge)          pk.challenge          = b64url(pk.challenge);
  if (pk.allowCredentials)   pk.allowCredentials   = pk.allowCredentials.map(c => ({ ...c, id: b64url(c.id) }));
}

function bufToB64url(buf) {
  const bytes = new Uint8Array(buf);
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=/g, '');
}

function encodeCredential(cred) {
  return {
    id:    cred.id,
    rawId: bufToB64url(cred.rawId),
    type:  cred.type,
    response: {
      attestationObject: bufToB64url(cred.response.attestationObject),
      clientDataJSON:    bufToB64url(cred.response.clientDataJSON),
      transports:        cred.response.getTransports ? cred.response.getTransports() : [],
    },
    extensions: cred.getClientExtensionResults ? cred.getClientExtensionResults() : {},
  };
}

function encodeAssertion(assertion) {
  return {
    id:    assertion.id,
    rawId: bufToB64url(assertion.rawId),
    type:  assertion.type,
    response: {
      authenticatorData: bufToB64url(assertion.response.authenticatorData),
      clientDataJSON:    bufToB64url(assertion.response.clientDataJSON),
      signature:         bufToB64url(assertion.response.signature),
      userHandle: assertion.response.userHandle
        ? bufToB64url(assertion.response.userHandle)
        : null,
    },
    extensions: assertion.getClientExtensionResults ? assertion.getClientExtensionResults() : {},
  };
}

// ── Boot ──────────────────────────────────────────────────────────────────────
document.addEventListener('DOMContentLoaded', () => transition('INIT'));
