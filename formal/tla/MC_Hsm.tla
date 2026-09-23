------------------------------ MODULE MC_Hsm ------------------------------
\* SPDX-License-Identifier: Apache-2.0
\* Copyright (c) 2026 Martin Schröder <info@swedishembedded.com>
\*
\* The hierarchy the engine's own integration tests use, minus one leaf.
\*
\*   Top
\*    +- S                      (composite, Init -> C; handles "cancel")
\*    |   +- C                  (composite, Init -> L1)
\*    |   |   +- L1             ("a" -> L2, "d" -> L1 self-transition)
\*    |   |   +- L2             ("b" -> P)
\*    |   +- T                  (composite, Init -> P)
\*    |       +- P              ("e" -> X, a transition INTO a composite)
\*    |       +- X              (composite, Init -> R)
\*    |           +- R          ("c" -> L1, the deepest cross-subtree hop)
\*    +- D                      (terminal)
\*
\* Nothing here is decoration. L1 <-> L2 are siblings under a composite, so a
\* transition between them is where an engine wrongly exits their parent. L2
\* -> P and R -> L1 cross subtrees at depth 1, so their LCA is S rather than
\* anybody's parent. "cancel" is handled at S and fires from any leaf, which is
\* the inherited transition whose exits start at the LEAF and not at S. "e"
\* targets a composite, so the drill has to finish the job. "z" is handled
\* nowhere, so it walks the whole chain and is refused at the root.

EXTENDS Integers

CONSTANTS ExitFromActiveLeaf, GenuineLca, SelfTransitionIsExternal,
          RestoreLeafOnly, DrillGuard, MalformedInit

MCStates == {"Top", "S", "C", "L1", "L2", "T", "P", "X", "R", "D"}
MCEvents == {"a", "b", "c", "d", "e", "cancel", "z"}
MCNoInit == "-"

MCSuper ==
    [ s \in MCStates |->
        IF s = "Top" THEN "Top"
        ELSE IF s \in {"S", "D"} THEN "Top"
        ELSE IF s \in {"C", "T"} THEN "S"
        ELSE IF s \in {"L1", "L2"} THEN "C"
        ELSE IF s \in {"P", "X"} THEN "T"
        ELSE "X" ]

\* The initial transitions of the composites. `MalformedInit` gives two LEAVES
\* an `Init` naming each other -- a machine that does not honour the trait's
\* "initial must be a proper descendant" contract. Nothing in Rust's type
\* system stops that, which is why it is a fault the model gets to inject
\* rather than an assumption it gets to make.
MCInitOf ==
    [ s \in MCStates |->
        IF s = "S" THEN "C"
        ELSE IF s = "C" THEN "L1"
        ELSE IF s = "T" THEN "P"
        ELSE IF s = "X" THEN "R"
        ELSE IF MalformedInit /\ s = "L1" THEN "L2"
        ELSE IF MalformedInit /\ s = "L2" THEN "L1"
        ELSE MCNoInit ]

Tran(t) == [ kind |-> "tran",    target |-> t ]
Sup     == [ kind |-> "super",   target |-> "Top" ]
Ign     == [ kind |-> "ignored", target |-> "Top" ]

MCHandler ==
    [ s \in MCStates |->
        [ e \in MCEvents |->
            IF s = "L1" THEN
                 IF e = "a" THEN Tran("L2")
                 ELSE IF e = "d" THEN Tran("L1")
                 ELSE Sup
            ELSE IF s = "L2" THEN IF e = "b" THEN Tran("P") ELSE Sup
            ELSE IF s = "P"  THEN IF e = "e" THEN Tran("X") ELSE Sup
            ELSE IF s = "R"  THEN IF e = "c" THEN Tran("L1") ELSE Sup
            ELSE IF s = "S"  THEN IF e = "cancel" THEN Tran("D") ELSE Sup
            ELSE IF s \in {"C", "T", "X"} THEN Sup
            ELSE Ign ] ]

\* What `Machine::all_states()` enumerates, and therefore what a snapshot may
\* name: every state but the implicit root. sven's own machines do exactly
\* this -- `ReactiveAgentMachine` lists `Session`, a composite, alongside its
\* two leaves.
MCRestorable == MCStates \ {"Top"}

VARIABLES active, entered, drilling, drillSeen, initialized, anomaly

INSTANCE Hsm
    WITH States    <- MCStates,
         Root      <- "Top",
         Terminal  <- {"D"},
         Super     <- MCSuper,
         Initial   <- "S",
         InitOf    <- MCInitOf,
         NoInit    <- MCNoInit,
         Events    <- MCEvents,
         Handler   <- MCHandler,
         Restorable <- MCRestorable

============================================================================
