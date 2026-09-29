#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
#
# Runs every TLA+ model in formal/tla/ and checks each one landed where it was
# supposed to.
#
# # Some models are supposed to FAIL
#
# A suite in which every check passes cannot tell you whether the checks have
# any teeth. Several specs here are parameterised by a design decision the
# engine actually made, and are run BOTH ways: once with the decision it took
# (expected to pass) and once with the plausible alternative it rejected
# (expected to fail, on a named property). An expected failure that starts
# passing is reported as a failure of this suite, because it means the spec
# stopped distinguishing the two designs.
#
# So each entry declares its expectation, and the runner compares against it
# rather than against "TLC exited 0".
#
# # Three kinds of expected failure, kept apart
#
# `violates:` is a design the code REJECTED, pinned down so the rejection
# stays justified. `gap:` is a property the code DOES NOT SATISFY TODAY and
# that is tracked as work -- a known hole, checked in so it cannot be quietly
# forgotten and so the day it starts passing is visible. `tradeoff:` is a
# property the code gives up ON PURPOSE in exchange for something else, priced
# by a sibling configuration that shows what keeping it would look like. All
# three run the same way and are reported differently, because they mean
# different things: a decision, a debt, and a price.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
jar="${here}/tools/tla2tools.jar"
specs="${here}/tla"
work="${TMPDIR:-/tmp}/sven-formal-tla.$$"

if ! command -v java >/dev/null 2>&1; then
    echo "[formal/tla] SKIPPED: no java on PATH; TLC needs a JRE 11 or newer."
    echo "[formal/tla] This is a note, not a silent pass."
    exit 0
fi

bash "${here}/fetch-tools.sh"
mkdir -p "$work"
trap 'rm -rf "$work"' EXIT

# module|config|expectation|what it establishes
#
# expectation is `pass`, `violates:<PropertyName>` for a design the engine
# rejected, `gap:<PropertyName>` for a hole it has not closed, or
# `tradeoff:<PropertyName>` for one it gave up deliberately.
suite=(
  "MC_Hsm|Hsm.cfg|pass|entry/exit discipline, LCA, drill and restore hold for every event in every reachable configuration"
  "MC_Hsm|HsmExitFromSource.cfg|violates:ConfigurationIsTheAncestorChain|exiting from the handling state instead of the active leaf leaves substates entered"
  "MC_Hsm|HsmLcaShortcut.cfg|violates:CommonAncestorUndisturbed|taking the source's parent as the LCA exits and re-enters ancestors a transition shares"
  "MC_Hsm|HsmLocalSelfTransition.cfg|violates:SelfTransitionRestartsItsState|a self-transition treated as local runs neither its exit nor its entry action"
  "MC_Hsm|HsmRestoreAnyState.cfg|violates:RestsWhereItCanRun|resuming into any enumerated state puts the machine in a composite no dispatch can produce"
  "MC_Hsm|HsmUnguardedDrill.cfg|violates:DispatchTerminates|an unguarded Init drill never returns on a machine whose initial transitions form a cycle"
  "MC_Hsm|HsmGuardedDrill.cfg|pass|the drill guard bounds that same malformed machine"
  "Submachine|Submachine.cfg|pass|a child that finishes is dropped and announced to its parent exactly once"
  "Submachine|SubmachineChildBornDone.cfg|pass|including a child its own initial transition finished"
  "Submachine|SubmachineNoCompleteOnInstall.cfg|violates:TerminalChildIsNotLeftInstalled|looking for completion only after an event leaves a finished child installed"
  "Submachine|SubmachineDoneChildKeepsReceiving.cfg|violates:NoEventReachesAFinishedChild|and routes the next event into a machine that had already ended"
  "Submachine|SubmachineParentLeaves.cfg|gap:NoOrphanedChild|a parent that transitions away from the state owning a child leaves it live and first in line"
  "EffectDelivery|EffectDeliveryAsShipped.cfg|pass|a refused non-tool effect is answered, so the machine waiting on it can leave"
  "EffectDelivery|EffectDeliveryUnanswered.cfg|violates:NoSilentStall|a refused batch the machine is never told about leaves it waiting forever"
  "EffectDelivery|EffectDeliveryPerEffect.cfg|pass|and gating each effect on its own keeps the allowed ones running too"
  "EffectDelivery|EffectDeliveryBatchDrop.cfg|tradeoff:InnocentEffectSurvivesARefusal|the all-or-nothing batch drops an allowed effect because another in the same dispatch was refused"
)

# Workers: TLC's own default is 1. Capped rather than unbounded because the
# box is shared.
workers="${TLC_WORKERS:-$(( $(nproc 2>/dev/null || echo 4) > 8 ? 8 : 4 ))}"

failures=0
printf '%s\n' "[formal/tla] TLC with ${workers} workers"

for entry in "${suite[@]}"; do
    IFS='|' read -r module cfg expect what <<<"$entry"
    log="${work}/${module}.${cfg}.log"

    set +e
    ( cd "$specs" && java -XX:+UseParallelGC -cp "$jar" tlc2.TLC \
        -config "$cfg" -workers "$workers" -cleanup \
        -metadir "${work}/${module}.${cfg}.states" "$module" ) >"$log" 2>&1
    status=$?
    set -e

    states="$(grep -oE '[0-9]+ distinct states found' "$log" | tail -1 | cut -d' ' -f1)"
    states="${states:-0}"

    case "$expect" in
        pass)
            if [ "$status" -eq 0 ] && grep -q "No error has been found" "$log"; then
                printf '  ok        %-38s %7s states  %s\n' "$cfg" "$states" "$what"
            else
                printf '  FAILED    %-38s %7s states  %s\n' "$cfg" "$states" "$what"
                sed -n '/^Error/,$p' "$log" | head -40
                failures=$((failures + 1))
            fi
            ;;
        violates:*|gap:*|tradeoff:*)
            want="${expect#*:}"
            case "${expect%%:*}" in
                gap)      label="ok (GAP) " ;;
                tradeoff) label="ok (COST)" ;;
                *)        label="ok (neg) " ;;
            esac
            # An invariant violation names the invariant. A temporal-property
            # violation does not -- TLC only says "Temporal properties were
            # violated" -- so for those the name is pinned by requiring the
            # configuration to declare exactly one PROPERTY, and requiring it
            # to be the expected one. A config that grows a second property
            # fails here rather than quietly accepting either violation.
            declared="$(grep -cE '^[[:space:]]*PROPERTY[[:space:]]' "${specs}/${cfg}" || true)"
            if [ "$status" -ne 0 ] && {
                   grep -qE "${want} is violated" "$log" ||
                   { grep -q "Temporal properties were violated" "$log" &&
                     [ "$declared" -eq 1 ] &&
                     grep -qE "^[[:space:]]*PROPERTY[[:space:]]+${want}[[:space:]]*$" "${specs}/${cfg}"; }
               }; then
                printf '  %s %-38s %7s states  %s\n' "$label" "$cfg" "$states" "$what"
            else
                printf '  FAILED    %-38s %7s states  expected a %s violation and did not get one\n' \
                    "$cfg" "$states" "$want"
                case "${expect%%:*}" in
                    gap)
                        echo "            (this is a tracked gap that now passes -- promote it to a pass entry)" ;;
                    tradeoff)
                        echo "            (a deliberate trade-off just started holding -- the design changed; re-read the spec)" ;;
                esac
                tail -20 "$log"
                failures=$((failures + 1))
            fi
            ;;
        *)
            printf '  FAILED    %-38s bad expectation %q in the suite table\n' "$cfg" "$expect"
            failures=$((failures + 1))
            ;;
    esac
done

if [ "$failures" -ne 0 ]; then
    echo "[formal/tla] ${failures} model(s) did not land where the suite says they should"
    exit 1
fi
gaps="$(printf '%s\n' "${suite[@]}" | grep -c '|gap:' || true)"
costs="$(printf '%s\n' "${suite[@]}" | grep -c '|tradeoff:' || true)"
echo "[formal/tla] ${#suite[@]} models checked, all as expected (${gaps} tracked gap(s), ${costs} priced trade-off(s))"
