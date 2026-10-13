# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Swedish Embedded AB implements formally validated control-flow architectures
# for its clients. If your team needs expertise in designing and model-checking
# hierarchical state-machine kernels, you can procure our services by sending
# an email to info@swedishembedded.com.
"""Figures of the paper, drawn from the architecture they describe.

    python figures.py <out dir>

Needs matplotlib (use a venv separate from the workspace). Every figure is
checked for overlapping text and text outside the image, and the script exits
with an error naming each offender, so none can ship unnoticed.
"""
import sys
import matplotlib
import matplotlib.text
matplotlib.use("Agg")
import matplotlib.pyplot as plt
from matplotlib.patches import FancyBboxPatch, FancyArrowPatch

OUT = sys.argv[1]
INK, MUTE, GRID = "#1b2a3a", "#6b7886", "#e3e8ee"
NAVY, TEAL, AMBER, RED, SLATE, GREEN = "#17324d", "#0f9d8a", "#e0a030", "#c8503c", "#9aa7b5", "#3a7d44"

plt.rcParams.update({
    "font.family": "DejaVu Sans", "font.size": 10.5, "text.color": INK,
    "axes.edgecolor": SLATE, "axes.labelcolor": INK, "xtick.color": MUTE,
    "ytick.color": MUTE, "axes.spines.top": False, "axes.spines.right": False,
    "figure.facecolor": "white", "axes.facecolor": "white", "savefig.dpi": 200,
})


def title(fig, head, sub):
    fig.text(0.04, 0.965, head, fontsize=16, fontweight="bold", va="top", color=INK)
    fig.text(0.04, 0.915, sub, fontsize=10.5, va="top", color=MUTE)


def foot(fig, text):
    fig.text(0.04, 0.02, text, fontsize=8.3, color=MUTE, va="bottom")


PROBLEMS = []


def check_text(fig, name):
    """Record every pair of overlapping text boxes and every text box outside the figure."""
    fig.canvas.draw()
    renderer = fig.canvas.get_renderer()
    figbox = fig.bbox
    boxes = []
    for t in fig.findobj(matplotlib.text.Text):
        if t.get_visible() and t.get_text().strip():
            b = t.get_window_extent(renderer)
            if b.width > 0:
                boxes.append((t.get_text().replace("\n", " / ")[:40], b))
    for label, b in boxes:
        if b.x0 < figbox.x0 - 1 or b.x1 > figbox.x1 + 1 or b.y0 < figbox.y0 - 1 or b.y1 > figbox.y1 + 1:
            PROBLEMS.append(f"{name}: outside the image: {label!r}")
    for i, (la, a) in enumerate(boxes):
        for lb, b in boxes[i + 1:]:
            if a.x0 < b.x1 - 1 and b.x0 < a.x1 - 1 and a.y0 < b.y1 - 1 and b.y0 < a.y1 - 1:
                PROBLEMS.append(f"{name}: overlap: {la!r} and {lb!r}")


def save(fig, name):
    check_text(fig, name)
    fig.savefig(f"{OUT}/{name}.png")
    plt.close(fig)


def box(ax, x, y, w, h, head, body, color, fc="white", fs_head=10.5, fs_body=8.8):
    ax.add_patch(FancyBboxPatch((x, y), w, h, boxstyle="round,pad=0.02,rounding_size=0.12",
                                fc=fc, ec=color, lw=1.6))
    if head:
        ax.text(x + 0.15, y + h - 0.05, head, fontsize=fs_head, fontweight="bold",
                color=color, va="top")
    if body:
        off = 0.42 if head else 0.05
        ax.text(x + 0.15, y + h - off, body, fontsize=fs_body, color=INK, va="top",
                linespacing=1.45)


def arrow(ax, x0, y0, x1, y1, color=MUTE, style="-|>", lw=1.6, ls="-"):
    ax.add_patch(FancyArrowPatch((x0, y0), (x1, y1), arrowstyle=style, color=color,
                                 lw=lw, linestyle=ls, mutation_scale=14,
                                 shrinkA=2, shrinkB=2))


# 1. The layered architecture -------------------------------------------------
def fig_architecture():
    fig = plt.figure(figsize=(12, 7.0))
    title(fig, "One kernel, many surfaces",
          "Every surface drives the same HSM kernel; the purity boundary is the only seam")
    ax = fig.add_axes([0.02, 0.05, 0.96, 0.80]); ax.axis("off"); ax.set_xlim(0, 12); ax.set_ylim(0, 7)

    box(ax, 0.2, 5.4, 11.6, 1.3, "Surfaces (each its own binary)",
        "interactive TUI   ·   headless CI runner   ·   ACP server (IDEs)   ·   MCP server   ·   SDK applications", NAVY)
    box(ax, 0.2, 4.0, 11.6, 1.0, "Shared frontend layer",
        "SessionController: the one place a session is opened, rebuilt, gated", NAVY)
    box(ax, 0.2, 2.9, 11.6, 0.8, "Assembly (RuntimeBuilder)",
        "one construction site for every kernel; mode registry; feature presets", NAVY)

    box(ax, 0.2, 1.35, 5.5, 1.35, "HSM kernel (pure)",
        "state hierarchies · two-phase dispatch · typed effects\npermissions · audit · snapshots  —  no I/O anywhere", TEAL)
    box(ax, 6.3, 1.35, 5.5, 1.35, "Runtime (the active object)",
        "single consumer task · permission gate · effect hand-off\nin-flight registry · cancellation · child-run spawning", TEAL)
    box(ax, 0.2, 0.05, 11.6, 0.95, "Executors (all I/O lives here)",
        "LLM turns · tools · human questions · timers · verification — results re-enter the queue as events", AMBER)

    arrow(ax, 6.0, 5.38, 6.0, 5.05, NAVY)
    arrow(ax, 6.0, 3.98, 6.0, 3.75, NAVY)
    arrow(ax, 6.0, 2.88, 6.0, 2.72, NAVY)
    arrow(ax, 2.9, 1.33, 2.9, 1.02, TEAL)
    arrow(ax, 9.1, 1.33, 9.1, 1.02, TEAL)
    arrow(ax, 5.75, 2.02, 6.25, 2.02, SLATE, style="<|-|>", lw=1.2, ls=(0, (4, 3)))

    foot(fig, "Above the seam: deterministic, replayable, model-checkable. Below it: the only place the outside world is touched.")
    save(fig, "fig1-architecture")


# 2. A state hierarchy with an LCA transition ---------------------------------
def fig_hierarchy():
    fig = plt.figure(figsize=(12, 6.4))
    title(fig, "A hierarchy earns its keep on every transition",
          "the reactive agent machine: two leaves under Session; the cancel event is handled once, at Session")
    ax = fig.add_axes([0.02, 0.05, 0.96, 0.80]); ax.axis("off"); ax.set_xlim(0, 12); ax.set_ylim(0, 6.4)

    box(ax, 4.6, 5.3, 2.8, 0.85, "Top (root)", "", NAVY, fs_head=10)
    box(ax, 4.2, 3.9, 3.6, 0.9, "Session", "handles UserCancelled for every child", NAVY, fs_head=10)
    box(ax, 2.4, 2.3, 2.6, 0.9, "Idle", "waits for the next user message", TEAL, fs_head=10)
    box(ax, 7.0, 2.3, 2.6, 0.9, "Generating", "LLM turn + tool loop, in-state", TEAL, fs_head=10)
    box(ax, 0.4, 0.35, 4.6, 1.35, "Transition  Generating → Idle",
        "LCA = Session (their closest shared ancestor)\nexit: Generating only   ·   enter: nothing\nSession survives: its handling is never re-run", AMBER, fs_head=10)
    box(ax, 6.6, 0.35, 5.0, 1.35, "Event  UserCancelled in Generating",
        "no handler in Generating → propagates up to Session\none handler serves both leaves (behavioural inheritance)\nno duplicate cancel logic in Idle or Generating", GREEN, fs_head=10)

    arrow(ax, 6.0, 5.28, 6.0, 4.83, NAVY)
    arrow(ax, 5.2, 3.88, 3.7, 3.23, NAVY)
    arrow(ax, 6.8, 3.88, 8.3, 3.23, NAVY)

    # the transition arc from Generating to Idle, routed over the top of Session
    ax.plot([8.3, 8.7, 3.3], [3.25, 4.95, 4.95], color=AMBER, lw=1.8, ls=(0, (5, 3)))
    arrow(ax, 3.3, 4.95, 3.7, 3.32, AMBER)
    ax.text(10.05, 4.95, "LCA = Session (exclusive):",
            fontsize=8.5, color=AMBER, ha="center", va="center")
    ax.text(10.05, 4.6, "exit Generating, enter Idle,\nSession is never re-entered",
            fontsize=8.5, color=AMBER, ha="center", va="center")

    foot(fig, "Between dispatches the machine is exactly the active leaf and its ancestors - the invariant the model checker proves, not a comment.")
    save(fig, "fig2-hierarchy")


# 3. The two-phase dispatch ---------------------------------------------------
def fig_dispatch():
    fig = plt.figure(figsize=(12, 6.8))
    title(fig, "The two-phase dispatch algorithm",
          "one algorithm drives every machine in the system; its whole job is to keep the configuration invariant")
    ax = fig.add_axes([0.02, 0.05, 0.96, 0.80]); ax.axis("off"); ax.set_xlim(0, 12); ax.set_ylim(0, 6.8)

    box(ax, 0.3, 5.4, 5.4, 1.1, "Phase 1: find the handler",
        "from the active leaf, ask the state; while it answers\n\"ask my parent\", climb the Super chain", NAVY, fs_head=10.5)
    box(ax, 6.3, 5.4, 5.4, 1.1, "Four possible answers",
        "Transition(target)  ·  Handled(effects)\nIgnored  ·  Super(parent)", SLATE, fs_head=10.5)

    box(ax, 0.3, 3.5, 3.4, 1.5, "Compute the LCA",
        "walk both ancestor chains;\nno depth-counting shortcut\nself-transition: parent as LCA\n(full exit + re-enter)", NAVY, fs_head=10)
    box(ax, 4.1, 3.5, 3.4, 1.5, "Exit, bottom-up",
        "from the active leaf up to,\nbut not including, the LCA\neffects collected in order", TEAL, fs_head=10)
    box(ax, 7.9, 3.5, 3.8, 1.5, "Transition action, then enter",
        "action effects fire between the\nexits and the entries; entries\nrun top-down from below the LCA\nto the target", TEAL, fs_head=10)

    box(ax, 0.3, 1.7, 5.4, 1.2, "Drill into composites",
        "the target may be a composite: fire its Init transition,\ndescend, repeat until a leaf; a visited-set guards a\nmalformed machine whose initial transitions cycle", NAVY, fs_head=10)
    box(ax, 6.3, 1.7, 5.4, 1.2, "One audit record per dispatch",
        "from-state, to-state, event kind, effects, rationale,\nprincipal - appended to the trail before the next event\n(the event-sourcing spine)", SLATE, fs_head=10)

    box(ax, 2.2, 0.15, 7.6, 0.9, "Effects handed to the permission gate, then to the executors",
        "the machine never acts on the world itself; results come back later, as events", AMBER, fs_head=10)

    arrow(ax, 5.75, 5.95, 6.25, 5.95, NAVY)
    arrow(ax, 6.5, 5.38, 2.9, 5.05, SLATE)
    arrow(ax, 3.75, 4.25, 4.05, 4.25, NAVY)
    arrow(ax, 7.55, 4.25, 7.85, 4.25, TEAL)
    arrow(ax, 9.8, 3.48, 5.2, 2.95, TEAL)
    arrow(ax, 3.0, 1.68, 4.0, 1.08, NAVY)
    foot(fig, "Phase 1 is behavioural inheritance; phase 2 is the exit-action / transition-action / entry-action ordering the UML semantics prescribes.")
    save(fig, "fig3-dispatch")


# 4. The effect lifecycle -----------------------------------------------------
def fig_effect():
    fig = plt.figure(figsize=(12, 6.6))
    title(fig, "The life of one tool call",
          "the machine proposes; the permission gate disposes, call by call; the executor acts; the result returns as an event")
    ax = fig.add_axes([0.02, 0.05, 0.96, 0.80]); ax.axis("off"); ax.set_xlim(0, 12); ax.set_ylim(0, 6.6)

    box(ax, 0.3, 5.3, 3.6, 1.1, "Pure transition",
        "returns a Reaction: state change\n+ typed Effects, here a CallTool\n(no I/O has happened yet)", NAVY, fs_head=10)
    box(ax, 4.35, 5.3, 3.4, 1.1, "Permission gate",
        "per-state policy + already-approved\nset → one of three dispositions\n(one choke point, every effect)", SLATE, fs_head=10)
    box(ax, 8.1, 5.3, 3.6, 1.1, "Executor",
        "performs the I/O on its own task\n(LLM, tool, user, timer, verifier)\nposts results through the EventSink", AMBER, fs_head=10)

    box(ax, 0.3, 3.2, 3.5, 1.35, "Allowed",
        "handed to the executor at once;\nthe call id joins the in-flight set\nso quiescence is not fooled", TEAL, fs_head=10)
    box(ax, 4.3, 3.2, 3.5, 1.35, "Forbidden",
        "never runs; a synthesized failure\nevent is emitted instead, so the\nmachine sees the refusal as a\nnormal tool result (no silent stall)", RED, fs_head=10)
    box(ax, 8.3, 3.2, 3.5, 1.35, "Needs approval",
        "an approval-request event is emitted;\nthe call waits; a person answers;\nthe approved call is issued as proposed", AMBER, fs_head=10)

    box(ax, 2.2, 1.5, 7.6, 0.95, "Result events re-enter the single queue",
        "ToolSucceeded / ToolFailed / LlmTurnComplete / HumanApproved / Timeout ...\nthe machine is dispatched again only when the queue offers the next event", NAVY, fs_head=10)
    box(ax, 2.2, 0.15, 7.6, 0.95, "Run-to-completion",
        "one consumer task drains the queue; dispatches never interleave; the audit trail records every one of them", TEAL, fs_head=10)

    arrow(ax, 3.95, 5.85, 4.3, 5.85, NAVY)
    arrow(ax, 7.8, 5.85, 8.05, 5.85, SLATE)
    arrow(ax, 9.9, 5.28, 9.9, 4.58, SLATE)
    arrow(ax, 2.1, 5.28, 2.1, 4.58, TEAL)
    arrow(ax, 6.05, 5.28, 6.05, 4.58, RED)
    arrow(ax, 2.0, 3.18, 3.6, 2.48, TEAL)
    arrow(ax, 6.05, 3.18, 6.05, 2.48, NAVY)
    arrow(ax, 10.05, 3.18, 8.5, 2.48, AMBER)
    arrow(ax, 6.0, 1.48, 6.0, 1.12, NAVY)
    foot(fig, "Non-tool effects pass the same gate as one all-or-nothing batch (Section 4.3); a refused one is answered with its own failure event.")
    save(fig, "fig4-effect")


# 5. The validation taxonomy --------------------------------------------------
def fig_validation():
    fig = plt.figure(figsize=(12, 6.2))
    title(fig, "A suite in which everything passes checks nothing",
          "16 TLC configurations on 2026-10-13: 6 must pass, 8 must fail on a named property, 1 tracked gap, 1 priced trade-off")
    ax = fig.add_axes([0.02, 0.05, 0.96, 0.80]); ax.axis("off"); ax.set_xlim(0, 12); ax.set_ylim(0, 6.2)

    box(ax, 0.3, 4.5, 5.5, 1.4, "Pass (6 configurations)",
        "the design as shipped must satisfy every property:\nancestor-chain invariant, exactly-once completion,\nno silent stall, guarded drill termination, ...", GREEN, fs_head=10)
    box(ax, 6.2, 4.5, 5.5, 1.4, "Rejected design (8 configurations)",
        "the plausible alternative the code turned down, run\nboth ways: the shipped design passes, the alternative\nmust fail on its own named property", RED, fs_head=10)

    box(ax, 0.3, 2.6, 5.5, 1.4, "Tracked gap (1)",
        "a property the code does not satisfy today, kept in\nthe suite and reported separately: Submachine's\norphaned child - closing it is a semantic decision", AMBER, fs_head=10)
    box(ax, 6.2, 2.6, 5.5, 1.4, "Priced trade-off (1)",
        "a property given up on purpose: the all-or-nothing\nnon-tool batch drops an allowed effect when a sibling\nis refused - the sibling configuration shows the price", SLATE, fs_head=10)

    box(ax, 0.3, 0.6, 11.4, 1.5, "What the models found",
        "the unbounded Init drill (a machine whose initial transitions cycle hung the agent silently - now guarded),\nresume-into-composite (a snapshot could place the machine where no dispatch ever rests it - now refused),\nthe refusal stall (an unanswered refused batch left the machine waiting forever - now answered)", NAVY, fs_head=10)

    foot(fig, "Every model names the code it models; a model that has drifted from the implementation is worse than no model, because it still passes.")
    save(fig, "fig5-validation")


fig_architecture()
fig_hierarchy()
fig_dispatch()
fig_effect()
fig_validation()

if PROBLEMS:
    print("\n".join(PROBLEMS))
    sys.exit(f"figures.py: {len(PROBLEMS)} layout problem(s), see above")
print(f"figures.py: 5 figures written to {OUT}")
