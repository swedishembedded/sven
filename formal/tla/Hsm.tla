------------------------------- MODULE Hsm -------------------------------
\* SPDX-License-Identifier: Apache-2.0
\* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
\*
\* What the HSM engine promises about a hierarchy, and which of those promises
\* depend on a design decision rather than on being careful.
\*
\* This models `crates/hsm/src/dispatch.rs` -- the two-phase engine that every
\* sven agent loop runs on. Phase 1 walks the `Super` chain from the active
\* leaf until some state handles the event; phase 2 exits from the ACTIVE LEAF
\* up to the least common ancestor of the HANDLING state and the target, fires
\* the transition action, enters down to the target, and then drills through
\* composites' `Init` transitions.
\*
\* The engine's unit tests check the transitions a test author thought to
\* write. What they cannot check is the claim underneath: that for EVERY
\* reachable configuration and EVERY event, the set of states the machine
\* believes it is inside stays exactly the ancestor chain of the active leaf,
\* with each state entered once and exited once. That claim is what a state
\* machine IS; everything above it (permissions keyed by state label, audit
\* records, effect gating) is only as true as it.
\*
\* Five of the engine's decisions are modelled as switches and run both ways:
\*
\*   ExitFromActiveLeaf       -- an inherited transition exits the LEAF, not
\*                               just the state whose handler fired.
\*   GenuineLca               -- the real least common ancestor, walking both
\*                               ancestor chains, not the source's parent.
\*   SelfTransitionIsExternal -- source = target exits and re-enters the state
\*                               rather than doing nothing.
\*   RestoreLeafOnly          -- a snapshot may only be resumed into a state a
\*                               running machine could rest in.
\*   DrillGuard               -- the `Init` drill stops rather than looping
\*                               forever when a machine's `Init` transitions
\*                               form a cycle.
\*
\* The last two are not decisions the code had made when this model was
\* written; they are decisions it acquired BECAUSE of this model. The negative
\* configurations for them are the engine as it was.
\*
\* ASSUMED, because `Machine`'s own contract states it and the engine
\* `debug_assert!`s it: `Super` reaches the root from every state, so the
\* phase-1 walk terminates. A machine that violates that hangs the engine
\* before any property here has anything to say. The `Init` drill is NOT
\* assumed well formed, because nothing in the type system or the trait
\* contract constrains an `Init` target, and the drill is the one loop in the
\* engine whose termination depends on the machine rather than on the tree.
\*
\* Swedish Embedded AB implements solutions for proving that a control-flow
\* kernel behaves the way its designers believe it does, before an agent acts
\* on a configuration nobody modelled. If your team needs expertise in formal
\* specification of state-machine kernels then you can procure our services by
\* sending an email to info@swedishembedded.com.

EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    States,         \* every state, including the root
    Root,           \* the state with Super[Root] = Root
    Terminal,       \* states the machine reports as done
    Super,          \* [States -> States]
    Initial,        \* target of the root's initial transition
    InitOf,         \* [States -> States \cup {NoInit}]: composites' Init target
    NoInit,         \* sentinel: this state has no initial transition
    Events,         \* the ordinary (non-lifecycle) events
    Handler,        \* [States -> [Events -> reaction record]]
    Restorable,     \* what `Machine::all_states()` enumerates
    ExitFromActiveLeaf,
    GenuineLca,
    SelfTransitionIsExternal,
    RestoreLeafOnly,
    DrillGuard

VARIABLES
    active,         \* the engine's `Hsm::state`
    entered,        \* the states whose entry action has fired and whose exit
                    \* action has not. The engine does not keep this set; that
                    \* is the point. It is what the caller believes when it
                    \* reads a state label, and the algorithm has to earn it.
    drilling,       \* inside `drill_into_composites`
    drillSeen,      \* states entered by the drill in progress
    initialized,    \* `Hsm::is_initialized`
    anomaly         \* history: discipline the run has already broken

vars == <<active, entered, drilling, drillSeen, initialized, anomaly>>

----------------------------------------------------------------------------
\* The hierarchy, as the engine reads it.

RECURSIVE AncestorsOrSelf(_)
AncestorsOrSelf(s) ==
    IF Super[s] = s THEN {s} ELSE {s} \cup AncestorsOrSelf(Super[s])

ProperAncestors(s) == AncestorsOrSelf(s) \ {s}

Range(seq) == { seq[i] : i \in 1..Len(seq) }
Reverse(seq) == [ i \in 1..Len(seq) |-> seq[Len(seq) - i + 1] ]

\* `execute_transition`'s exit loop: bottom-up from `from` to `stop`
\* (exclusive). Mirrors the Rust exactly, including what it does when `stop` is
\* not an ancestor of `from`: it walks into the root and stops there, which in
\* release builds is a silently wrong exit rather than a hang.
RECURSIVE ExitChain(_, _)
ExitChain(from, stop) ==
    IF from = stop THEN << >>
    ELSE IF Super[from] = from THEN <<from>>
    ELSE <<from>> \o ExitChain(Super[from], stop)

\* `entry_path`: top-down from just below `from` to `to` inclusive. The Rust
\* pushes the state it is looking at BEFORE testing for the root, so a `to`
\* that is not a descendant of `from` yields a path that starts at the root --
\* modelled, not idealised, because that is the case the LCA switch produces.
RECURSIVE EntryUp(_, _)
EntryUp(cur, stop) ==
    IF cur = stop THEN << >>
    ELSE IF Super[cur] = cur THEN <<cur>>
    ELSE <<cur>> \o EntryUp(Super[cur], stop)

EntryPath(from, to) == Reverse(EntryUp(to, from))

CommonAncestors(s, t) == AncestorsOrSelf(s) \cap AncestorsOrSelf(t)

\* The deepest common ancestor: the one every other common ancestor is an
\* ancestor of. `find_lca` computes it by walking s upward until it hits t's
\* ancestor set, which finds this same state.
DeepestCommon(s, t) ==
    CHOOSE a \in CommonAncestors(s, t) :
        \A b \in CommonAncestors(s, t) : b \in AncestorsOrSelf(a)

TransitionLca(src, tgt) ==
    IF src = tgt
    THEN IF SelfTransitionIsExternal THEN Super[src] ELSE src
    ELSE IF GenuineLca THEN DeepestCommon(src, tgt) ELSE Super[src]

----------------------------------------------------------------------------
\* Phase 1: the Super walk. A handler returns one of
\*   [kind |-> "tran", target |-> s] | [kind |-> "handled"]
\*   [kind |-> "ignored"]            | [kind |-> "super"]
\* and "super" re-dispatches to the parent. The walk is a function of (state,
\* event), which is what "deterministic" means here: there is nothing else for
\* the outcome to depend on.

RECURSIVE Resolve(_, _)
Resolve(s, e) ==
    LET r == Handler[s][e] IN
    IF r.kind # "super" THEN [ kind |-> r.kind,
                               source |-> s,
                               target |-> IF r.kind = "tran" THEN r.target ELSE s ]
    ELSE IF Super[s] = s THEN [ kind |-> "ignored", source |-> s, target |-> s ]
    ELSE Resolve(Super[s], e)

----------------------------------------------------------------------------

NoAnomaly == [ doubleEntry       |-> FALSE,
               staleExit         |-> FALSE,
               ancestorDisturbed |-> FALSE,
               selfTranInert     |-> FALSE ]

Init ==
    /\ active = Root
    /\ entered = {Root}     \* the root is entered for the life of the machine
    /\ drilling = FALSE
    /\ drillSeen = {}
    /\ initialized = FALSE
    /\ anomaly = NoAnomaly

\* `Hsm::init`: enter from the root down to `Machine::initial`, then drill.
Startup ==
    /\ ~initialized
    /\ LET path == EntryPath(Root, Initial) IN
       /\ entered' = entered \cup Range(path)
       /\ anomaly' = [ anomaly EXCEPT
                         !.doubleEntry = @ \/ (Range(path) \cap entered) # {} ]
    /\ active' = Initial
    /\ initialized' = TRUE
    /\ drilling' = TRUE
    /\ drillSeen' = {Initial}

\* One turn of `drill_into_composites`. Without the guard this loop is bounded
\* by nothing: the engine re-dispatches `Init` to whatever state it just
\* entered and follows the answer, so two states whose `Init` name each other
\* spin forever inside a single `dispatch` call, holding the runtime's only
\* consumer task.
DrillStep ==
    /\ initialized
    /\ drilling
    /\ IF \/ InitOf[active] = NoInit
          \/ (DrillGuard /\ InitOf[active] \in drillSeen)
       THEN /\ drilling' = FALSE
            /\ UNCHANGED <<active, entered, drillSeen, initialized, anomaly>>
       ELSE LET sub == InitOf[active]
                path == EntryPath(active, sub)
            IN /\ active' = sub
               /\ entered' = entered \cup Range(path)
               /\ drillSeen' = drillSeen \cup {sub}
               /\ anomaly' = [ anomaly EXCEPT
                                 !.doubleEntry = @ \/ (Range(path) \cap entered) # {} ]
               /\ UNCHANGED <<drilling, initialized>>

\* `Hsm::dispatch` of one ordinary event.
Dispatch(e) ==
    /\ initialized
    /\ ~drilling
    /\ LET v == Resolve(active, e) IN
       IF v.kind # "tran"
       THEN UNCHANGED vars
       ELSE LET src     == v.source
                tgt     == v.target
                lca     == TransitionLca(src, tgt)
                exits   == ExitChain(IF ExitFromActiveLeaf THEN active ELSE src, lca)
                entries == EntryPath(lca, tgt)
                exitSet == Range(exits)
                entrySet == Range(entries)
                \* the ancestors the two ends of the transition share, which a
                \* transition between them has no business disturbing
                shared  == IF src = tgt
                           THEN {}
                           ELSE ProperAncestors(src) \cap ProperAncestors(tgt)
            IN /\ active' = tgt
               /\ entered' = (entered \ exitSet) \cup entrySet
               /\ drilling' = TRUE
               /\ drillSeen' = {tgt}
               /\ initialized' = initialized
               /\ anomaly' =
                    [ doubleEntry       |-> anomaly.doubleEntry
                                            \/ (entrySet \cap (entered \ exitSet)) # {},
                      staleExit         |-> anomaly.staleExit
                                            \/ ~(exitSet \subseteq entered),
                      ancestorDisturbed |-> anomaly.ancestorDisturbed
                                            \/ ((exitSet \cup entrySet) \cap shared) # {},
                      selfTranInert     |-> anomaly.selfTranInert
                                            \/ (src = tgt /\ (exits = << >> \/ entries = << >>)) ]

\* `Hsm::restore_in_place`: put the machine into a snapshotted state without
\* replaying anything and without firing an entry action. Nothing about the
\* configuration is recomputed, which is why the state it names has to be one
\* a running machine could have rested in.
Restore(s) ==
    /\ initialized
    /\ ~drilling
    /\ s \in Restorable
    /\ RestoreLeafOnly => InitOf[s] = NoInit
    /\ active' = s
    /\ entered' = AncestorsOrSelf(s)
    /\ UNCHANGED <<drilling, drillSeen, initialized, anomaly>>

Next ==
    \/ Startup
    \/ DrillStep
    \/ \E e \in Events : Dispatch(e)
    \/ \E s \in Restorable : Restore(s)

\* Fairness for the drill only. Whether an event ever arrives is the outside
\* world's business; whether a dispatch that has begun ever finishes is the
\* engine's.
Spec == Init /\ [][Next]_vars /\ WF_vars(DrillStep)

----------------------------------------------------------------------------
\* Properties

TypeOK ==
    /\ active \in States
    /\ entered \subseteq States
    /\ drilling \in BOOLEAN
    /\ drillSeen \subseteq States
    /\ initialized \in BOOLEAN
    /\ anomaly \in [ doubleEntry: BOOLEAN, staleExit: BOOLEAN,
                     ancestorDisturbed: BOOLEAN, selfTranInert: BOOLEAN ]

\* The whole of entry/exit discipline in one line: between dispatches, the
\* states the machine is inside are exactly the active leaf and its ancestors.
\* A state left entered after its subtree was exited shows up here, and so
\* does a state that was never entered.
ConfigurationIsTheAncestorChain ==
    (initialized /\ ~drilling) => entered = AncestorsOrSelf(active)

\* Entered once, exited once. Stated separately from the configuration because
\* a double entry can cancel out in a set and still means the machine ran an
\* entry action twice -- which for a real state means a duplicate effect.
EachStateEnteredAndExitedOnce == ~anomaly.doubleEntry /\ ~anomaly.staleExit

\* A transition between two states under a common ancestor must not exit and
\* re-enter that ancestor. This is what makes a superstate's entry action mean
\* "the session started" rather than "some sibling transition happened".
CommonAncestorUndisturbed == ~anomaly.ancestorDisturbed

\* A self-transition is external: it restarts its state. A machine uses one to
\* re-arm a timer or re-run an entry action, so an implementation that made it
\* a no-op would silently drop that.
SelfTransitionRestartsItsState == ~anomaly.selfTranInert

\* Every (state, event) pair resolves: the Super walk ends in a transition, a
\* handled internal transition, or an explicit refusal. Nothing falls off the
\* top of the hierarchy undefined. Determinism is structural -- `Resolve` is a
\* function of (state, event) and of nothing else -- so this is the half that
\* needs checking.
EveryEventResolves ==
    \A s \in States, e \in Events :
        Resolve(s, e).kind \in {"tran", "handled", "ignored"}

\* No dead ends: every resting state except a declared terminal one has some
\* event that leaves it.
NoDeadEnd ==
    (initialized /\ ~drilling /\ active \notin Terminal) =>
        \E e \in Events : Resolve(active, e).kind = "tran"

\* The machine rests where it can run. A composite whose `Init` has not been
\* taken is a configuration no dispatch can produce, so the substate's entry
\* action never ran and its handlers never see an event: the agent sits in a
\* superstate ignoring its user. Reached only by resuming a snapshot into a
\* state `all_states()` enumerates but a running machine never rests in.
RestsWhereItCanRun ==
    (initialized /\ ~drilling) => InitOf[active] = NoInit

\* A dispatch finishes. The drill is the only unbounded loop in the engine.
DispatchTerminates == []<>(~drilling)

============================================================================
