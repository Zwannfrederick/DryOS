------------------------------ MODULE DrySched ------------------------------
(***************************************************************************)
(* H1 Scheduler Protocol Formal Specification for DRY Kernel               *)
(*                                                                         *)
(* Models the packed AtomicU32(exec_state, park_token) per-task state      *)
(* machine across multiple CPUs with:                                      *)
(*  - Explicit 2-step runqueue publication (MarkReady vs PushRq)           *)
(*  - Explicit 2-step dequeue/work-stealing (DequeueTask vs TryDispatch)   *)
(*  - Explicit context-switch stack ownership window (stack_users)         *)
(*  - Preemption & cross-CPU migration while park_token = TOK_ARMED        *)
(*  - Concurrent dual wakeups (signal waker + spurious/timeout waker)      *)
(*  - Self-cancellation / timeout before commit_park                       *)
(*  - Asynchronous task termination & quiescent-epoch TCB reclamation      *)
(*  - Four toggleable protocol defects to reproduce counterexamples        *)
(***************************************************************************)
EXTENDS Naturals, Sequences, FiniteSets

CONSTANTS
    CPUs,                       \* Set of logical CPUs, e.g. {"c0", "c1", "c2"}
    BugEarlyReadyBeforeSwitch,  \* Defect 1: publish READY before stack switch finishes
    BugPushBeforeReady,         \* Defect 2: rq.push(T) before exec_state := READY
    BugPostSwitchBlindStore,    \* Defect 3: blind store in post_switch clobbers token/exit
    BugWakingBlindStore,        \* Defect 4: blind store in WAKING->READY clobbers token/exit
    RequireQuantumStep,         \* Fairness regime: >=1 task instruction before timer preempt
    AllowExternalKill           \* Enable asynchronous task termination / revocation

VARIABLES
    exec_state,     \* "READY"|"RUNNING"|"SW_PREEMPT"|"SW_PARK"|"BLOCKED"|"WAKING"|"EXITING"|"DEAD"
    park_token,     \* "TOK_NONE" | "TOK_ARMED" | "TOK_NOTIFIED"
    pc,             \* Task wait/work program counter
    cond,           \* Shared event condition / Notification signal bit (0 or 1)
    sig_stage,      \* Signal producer stage: "INIT" | "COND_SET" | "SENT"
    rq,             \* Per-CPU runqueue: [CPUs -> Seq({"T"})]
    cpu_curr,       \* Currently running task on each CPU: [CPUs -> {"T", "Idle"}]
    cpu_prev,       \* Outgoing task in context switch on each CPU: [CPUs -> {"T", "None"}]
    cpu_prev_mode,  \* Switch reason saved in CPU register: [CPUs -> {"NONE", "PREEMPT", "PARK", "EXIT"}]
    cpu_prev_tok,   \* Stale token snapshot at switch start (used only by Defect 3)
    cpu_popped,     \* Dequeued/stolen candidate before CAS(READY->RUNNING): [CPUs -> {"T", "None"}]
    post_stage,     \* Outgoing CPU post-switch publication stage: [CPUs -> {"NONE", "NEED_PUSH", "NEED_READY"}]
    waker_stage,    \* Waker publication stage: [{"w_sig", "w_spur"} -> {"IDLE", "IN_WAKING", "NEED_PUSH", "NEED_READY", "DONE"}]
    stack_users,    \* Set of CPUs physically executing on T's kernel stack right now
    slice_stepped,  \* Whether T executed >=1 instruction in its current dispatch slice
    terminated      \* Monotonic latch: TRUE once T entered EXITING

vars == <<
    exec_state, park_token, pc, cond, sig_stage,
    rq, cpu_curr, cpu_prev, cpu_prev_mode, cpu_prev_tok, cpu_popped,
    post_stage, waker_stage, stack_users, slice_stepped, terminated
>>

Wakers == {"w_sig", "w_spur"}

ExecStates == {"READY", "RUNNING", "SW_PREEMPT", "SW_PARK", "BLOCKED", "WAKING", "EXITING", "DEAD"}
ParkTokens == {"TOK_NONE", "TOK_ARMED", "TOK_NOTIFIED"}
PCStates   == {"PC_RUN", "PC_ARMED", "PC_CHECKED_ZERO", "PC_SEEN_ONE", "PC_IN_PARK", "PC_DONE"}

SeqContains(s, x) == \E i \in 1..Len(s) : s[i] = x
SeqCount(s, x)    == Cardinality({i \in 1..Len(s) : s[i] = x})

CountInRq ==
    LET F[S \in SUBSET CPUs] ==
        IF S = {} THEN 0
        ELSE LET c == CHOOSE x \in S : TRUE
             IN SeqCount(rq[c], "T") + F[S \ {c}]
    IN F[CPUs]

CountPopped == Cardinality({c \in CPUs : cpu_popped[c] = "T"})

CountEnqueuing ==
    Cardinality({c \in CPUs : post_stage[c] = "NEED_PUSH"}) +
    Cardinality({w \in Wakers : waker_stage[w] = "NEED_PUSH"})

InAnyRq == CountInRq > 0

-----------------------------------------------------------------------------
(* Initial State: T starts READY in one initial CPU's runqueue             *)
-----------------------------------------------------------------------------
Init ==
    /\ \E boot_cpu \in CPUs :
        rq = [c \in CPUs |-> IF c = boot_cpu THEN <<"T">> ELSE <<>>]
    /\ exec_state    = "READY"
    /\ park_token    = "TOK_NONE"
    /\ pc            = "PC_RUN"
    /\ cond          = 0
    /\ sig_stage     = "INIT"
    /\ cpu_curr      = [c \in CPUs |-> "Idle"]
    /\ cpu_prev      = [c \in CPUs |-> "None"]
    /\ cpu_prev_mode = [c \in CPUs |-> "NONE"]
    /\ cpu_prev_tok  = [c \in CPUs |-> "TOK_NONE"]
    /\ cpu_popped    = [c \in CPUs |-> "None"]
    /\ post_stage    = [c \in CPUs |-> "NONE"]
    /\ waker_stage   = [w \in Wakers |-> "IDLE"]
    /\ stack_users   = {}
    /\ slice_stepped = FALSE
    /\ terminated    = FALSE

-----------------------------------------------------------------------------
(* 1. Dequeue (local pop or cross-CPU work-steal) & Atomic Dispatch        *)
-----------------------------------------------------------------------------
\* Step D1: Idle CPU `c` pops from its own rq (c = src) or steals from `src` (c # src).
\* Only READY tasks in rq are stolen in real code, though rq removal itself happens before CAS.
DequeueTask(c, src) ==
    /\ cpu_curr[c] = "Idle"
    /\ cpu_prev[c] = "None"
    /\ cpu_popped[c] = "None"
    /\ post_stage[c] = "NONE"
    /\ Len(rq[src]) > 0
    /\ rq' = [rq EXCEPT ![src] = Tail(@)]
    /\ cpu_popped' = [cpu_popped EXCEPT ![c] = Head(rq[src])]
    /\ UNCHANGED <<exec_state, park_token, pc, cond, sig_stage, cpu_curr, cpu_prev,
                   cpu_prev_mode, cpu_prev_tok, post_stage, waker_stage,
                   stack_users, slice_stepped, terminated>>

\* Step D2: CPU `c` attempts CAS((READY, tok) -> (RUNNING, tok)) on popped task.
TryDispatchPopped(c) ==
    /\ cpu_popped[c] = "T"
    /\ cpu_popped' = [cpu_popped EXCEPT ![c] = "None"]
    /\ IF exec_state = "READY"
       THEN /\ exec_state' = "RUNNING"
            /\ cpu_curr' = [cpu_curr EXCEPT ![c] = "T"]
            /\ stack_users' = stack_users \cup {c}
            /\ slice_stepped' = FALSE
            /\ pc' = IF pc = "PC_IN_PARK" THEN "PC_RUN" ELSE pc
       ELSE \* CAS failed (e.g. task transitioned to EXITING, or BugPushBeforeReady!)
            /\ UNCHANGED <<exec_state, cpu_curr, stack_users, slice_stepped, pc>>
    /\ UNCHANGED <<park_token, cond, sig_stage, rq, cpu_prev, cpu_prev_mode,
                   cpu_prev_tok, post_stage, waker_stage, terminated>>

-----------------------------------------------------------------------------
(* 2. Running Task Wait / Cancel / Park Protocol Steps on CPU `c`          *)
-----------------------------------------------------------------------------
\* Task calls prepare_park(): arms park_token (preserving TOK_NOTIFIED if already set? No:
\* fresh prepare_park before checking cond sets TOK_ARMED so stale signals don't leak).
TaskPreparePark(c) ==
    /\ cpu_curr[c] = "T"
    /\ exec_state = "RUNNING"
    /\ pc = "PC_RUN"
    /\ park_token' = "TOK_ARMED"
    /\ pc' = "PC_ARMED"
    /\ slice_stepped' = TRUE
    /\ UNCHANGED <<exec_state, cond, sig_stage, rq, cpu_curr, cpu_prev, cpu_prev_mode,
                   cpu_prev_tok, cpu_popped, post_stage, waker_stage, stack_users, terminated>>

\* Task reads `cond` after SeqCst fence.
TaskCheckCond(c) ==
    /\ cpu_curr[c] = "T"
    /\ exec_state = "RUNNING"
    /\ pc = "PC_ARMED"
    /\ pc' = IF cond = 1 THEN "PC_SEEN_ONE" ELSE "PC_CHECKED_ZERO"
    /\ slice_stepped' = TRUE
    /\ UNCHANGED <<exec_state, park_token, cond, sig_stage, rq, cpu_curr, cpu_prev,
                   cpu_prev_mode, cpu_prev_tok, cpu_popped, post_stage, waker_stage,
                   stack_users, terminated>>

\* Task saw cond = 1 -> cancels park_token and finishes wait loop.
TaskFinishWait(c) ==
    /\ cpu_curr[c] = "T"
    /\ exec_state = "RUNNING"
    /\ pc = "PC_SEEN_ONE"
    /\ park_token' = "TOK_NONE"
    /\ pc' = "PC_DONE"
    /\ slice_stepped' = TRUE
    /\ UNCHANGED <<exec_state, cond, sig_stage, rq, cpu_curr, cpu_prev, cpu_prev_mode,
                   cpu_prev_tok, cpu_popped, post_stage, waker_stage, stack_users, terminated>>

\* Voluntary cancellation / local timeout before commit_park: clears token and retries loop.
TaskCancelBeforeCommit(c) ==
    /\ cpu_curr[c] = "T"
    /\ exec_state = "RUNNING"
    /\ pc = "PC_CHECKED_ZERO"
    /\ cond = 0 \* Only model non-blocking retry before cond is seen, keeping state graph focused
    /\ park_token' = "TOK_NONE"
    /\ pc' = "PC_RUN"
    /\ slice_stepped' = TRUE
    /\ UNCHANGED <<exec_state, cond, sig_stage, rq, cpu_curr, cpu_prev, cpu_prev_mode,
                   cpu_prev_tok, cpu_popped, post_stage, waker_stage, stack_users, terminated>>

\* Task calls commit_park():
\* - Fast path: if park_token = TOK_NOTIFIED, consume token and return immediately!
\* - Slow path: CAS((RUNNING, TOK_ARMED) -> (SW_PARK, TOK_NONE)) and enter context switch!
TaskCommitPark(c) ==
    /\ cpu_curr[c] = "T"
    /\ exec_state = "RUNNING"
    /\ pc = "PC_CHECKED_ZERO"
    /\ post_stage[c] = "NONE"
    /\ IF park_token = "TOK_NOTIFIED"
       THEN /\ park_token' = "TOK_NONE"
            /\ pc' = "PC_RUN"
            /\ slice_stepped' = TRUE
            /\ UNCHANGED <<exec_state, cpu_curr, cpu_prev, cpu_prev_mode, cpu_prev_tok>>
       ELSE /\ park_token = "TOK_ARMED"
            /\ exec_state' = "SW_PARK"
            /\ park_token' = "TOK_NONE"
            /\ pc' = "PC_IN_PARK"
            /\ cpu_curr' = [cpu_curr EXCEPT ![c] = "Idle"]
            /\ cpu_prev' = [cpu_prev EXCEPT ![c] = "T"]
            /\ cpu_prev_mode' = [cpu_prev_mode EXCEPT ![c] = "PARK"]
            /\ cpu_prev_tok' = [cpu_prev_tok EXCEPT ![c] = "TOK_NONE"]
            /\ slice_stepped' = TRUE
    /\ UNCHANGED <<cond, sig_stage, rq, cpu_popped, post_stage, waker_stage,
                   stack_users, terminated>>

\* Once PC_DONE, a running task keeps executing CPU-bound useful work (can still be preempted/migrated).
TaskRunDoneWork(c) ==
    /\ cpu_curr[c] = "T"
    /\ exec_state = "RUNNING"
    /\ pc = "PC_DONE"
    /\ ~slice_stepped
    /\ slice_stepped' = TRUE
    /\ UNCHANGED <<exec_state, park_token, pc, cond, sig_stage, rq, cpu_curr, cpu_prev,
                   cpu_prev_mode, cpu_prev_tok, cpu_popped, post_stage, waker_stage,
                   stack_users, terminated>>

-----------------------------------------------------------------------------
(* 3. Preemption & Two-Phase Post-Switch Publication                       *)
-----------------------------------------------------------------------------
PreemptTask(c) ==
    /\ cpu_curr[c] = "T"
    /\ exec_state = "RUNNING"
    /\ post_stage[c] = "NONE"
    /\ (RequireQuantumStep => slice_stepped)
    /\ cpu_curr' = [cpu_curr EXCEPT ![c] = "Idle"]
    /\ cpu_prev' = [cpu_prev EXCEPT ![c] = "T"]
    /\ cpu_prev_mode' = [cpu_prev_mode EXCEPT ![c] = "PREEMPT"]
    /\ cpu_prev_tok' = [cpu_prev_tok EXCEPT ![c] = park_token]
    /\ IF BugEarlyReadyBeforeSwitch
       THEN \* DEFECT 1: Mark READY and push to rq BEFORE context switch releases T's stack!
            /\ exec_state' = "READY"
            /\ rq' = [rq EXCEPT ![c] = Append(@, "T")]
       ELSE /\ exec_state' = "SW_PREEMPT"
            /\ UNCHANGED rq
    /\ UNCHANGED <<park_token, pc, cond, sig_stage, cpu_popped, post_stage,
                   waker_stage, stack_users, slice_stepped, terminated>>

\* If task was externally killed while RUNNING, CPU switches off its stack cleanly.
HandleKilledRunning(c) ==
    /\ cpu_curr[c] = "T"
    /\ exec_state = "EXITING"
    /\ post_stage[c] = "NONE"
    /\ cpu_curr' = [cpu_curr EXCEPT ![c] = "Idle"]
    /\ cpu_prev' = [cpu_prev EXCEPT ![c] = "T"]
    /\ cpu_prev_mode' = [cpu_prev_mode EXCEPT ![c] = "EXIT"]
    /\ UNCHANGED <<exec_state, park_token, pc, cond, sig_stage, rq, cpu_prev_tok,
                   cpu_popped, post_stage, waker_stage, stack_users, slice_stepped, terminated>>

\* Phase 1 of post_switch(c): CPU `c` has switched RSP off T's stack (`stack_users \ {c}`).
PostSwitchFinishStack(c) ==
    /\ cpu_prev[c] = "T"
    /\ cpu_prev' = [cpu_prev EXCEPT ![c] = "None"]
    /\ cpu_prev_mode' = [cpu_prev_mode EXCEPT ![c] = "NONE"]
    /\ stack_users' = stack_users \ {c}
    /\ CASE BugEarlyReadyBeforeSwitch /\ cpu_prev_mode[c] = "PREEMPT" ->
            \* Defect 1 already pushed T before switching stacks
            UNCHANGED <<exec_state, park_token, rq, post_stage>>

         [] cpu_prev_mode[c] = "EXIT" ->
            UNCHANGED <<exec_state, park_token, rq, post_stage>>

         [] cpu_prev_mode[c] = "PREEMPT" ->
            IF BugPushBeforeReady
            THEN \* DEFECT 2: Push T to rq BEFORE transitioning SW_PREEMPT -> READY!
                 /\ rq' = [rq EXCEPT ![c] = Append(@, "T")]
                 /\ post_stage' = [post_stage EXCEPT ![c] = "NEED_READY"]
                 /\ UNCHANGED <<exec_state, park_token>>
            ELSE IF BugPostSwitchBlindStore
            THEN \* DEFECT 3a: Blindly store (READY, stale_tok), clobbering concurrent TOK_NOTIFIED or EXITING!
                 /\ exec_state' = "READY"
                 /\ park_token' = cpu_prev_tok[c]
                 /\ post_stage' = [post_stage EXCEPT ![c] = "NEED_PUSH"]
                 /\ UNCHANGED rq
            ELSE \* CORRECT: CAS((SW_PREEMPT, tok) -> (READY, tok)) preserving current park_token!
                 IF exec_state = "SW_PREEMPT"
                 THEN /\ exec_state' = "READY"
                      /\ post_stage' = [post_stage EXCEPT ![c] = "NEED_PUSH"]
                      /\ UNCHANGED <<park_token, rq>>
                 ELSE \* Task transitioned to EXITING concurrently; do not enqueue!
                      UNCHANGED <<exec_state, park_token, rq, post_stage>>

         [] cpu_prev_mode[c] = "PARK" ->
            IF BugPostSwitchBlindStore
            THEN \* DEFECT 3b: Blindly store (BLOCKED, TOK_NONE), clobbering TOK_NOTIFIED or EXITING!
                 /\ exec_state' = "BLOCKED"
                 /\ park_token' = "TOK_NONE"
                 /\ UNCHANGED <<rq, post_stage>>
            ELSE \* CORRECT: Inspect atomic SW_PARK state via CAS!
                 IF exec_state # "SW_PARK"
                 THEN \* Task transitioned to EXITING concurrently
                      UNCHANGED <<exec_state, park_token, rq, post_stage>>
                 ELSE IF park_token = "TOK_NOTIFIED"
                 THEN \* Concurrent wakeup arrived during switch_to -> consume token & wake to READY!
                      IF BugPushBeforeReady
                      THEN /\ rq' = [rq EXCEPT ![c] = Append(@, "T")]
                           /\ post_stage' = [post_stage EXCEPT ![c] = "NEED_READY"]
                           /\ UNCHANGED <<exec_state, park_token>>
                      ELSE /\ exec_state' = "READY"
                           /\ park_token' = "TOK_NONE"
                           /\ post_stage' = [post_stage EXCEPT ![c] = "NEED_PUSH"]
                           /\ UNCHANGED rq
                 ELSE \* CAS((SW_PARK, TOK_NONE) -> (BLOCKED, TOK_NONE))
                      /\ exec_state' = "BLOCKED"
                      /\ park_token' = "TOK_NONE"
                      /\ UNCHANGED <<rq, post_stage>>
    /\ UNCHANGED <<pc, cond, sig_stage, cpu_curr, cpu_prev_tok, cpu_popped,
                   waker_stage, slice_stepped, terminated>>

\* Phase 2 of post_switch(c) (Correct order): Push READY task onto rq[c].
PostSwitchPushRq(c) ==
    /\ post_stage[c] = "NEED_PUSH"
    /\ rq' = [rq EXCEPT ![c] = Append(@, "T")]
    /\ post_stage' = [post_stage EXCEPT ![c] = "NONE"]
    /\ UNCHANGED <<exec_state, park_token, pc, cond, sig_stage, cpu_curr, cpu_prev,
                   cpu_prev_mode, cpu_prev_tok, cpu_popped, waker_stage,
                   stack_users, slice_stepped, terminated>>

\* Phase 2 of post_switch(c) (Defect 2 order): Late transition to READY after rq.push!
PostSwitchLateReady(c) ==
    /\ post_stage[c] = "NEED_READY"
    /\ post_stage' = [post_stage EXCEPT ![c] = "NONE"]
    /\ IF exec_state \in {"SW_PREEMPT", "SW_PARK"}
       THEN /\ exec_state' = "READY"
            /\ park_token' = IF exec_state = "SW_PARK" THEN "TOK_NONE" ELSE park_token
       ELSE UNCHANGED <<exec_state, park_token>>
    /\ UNCHANGED <<pc, cond, sig_stage, rq, cpu_curr, cpu_prev, cpu_prev_mode,
                   cpu_prev_tok, cpu_popped, waker_stage, stack_users,
                   slice_stepped, terminated>>

-----------------------------------------------------------------------------
(* 4. Signal Waker & Concurrent Spurious/Timeout Waker                     *)
-----------------------------------------------------------------------------
\* Step S1: Producer sets shared condition cond := 1 (before calling wakeup).
SignalSetCond ==
    /\ sig_stage = "INIT"
    /\ cond' = 1
    /\ sig_stage' = "COND_SET"
    /\ UNCHANGED <<exec_state, park_token, pc, rq, cpu_curr, cpu_prev, cpu_prev_mode,
                   cpu_prev_tok, cpu_popped, post_stage, waker_stage,
                   stack_users, slice_stepped, terminated>>

\* Step S2: Producer executes atomic wakeup(T) after SeqCst fence.
SignalWakeup ==
    /\ sig_stage = "COND_SET"
    /\ waker_stage["w_sig"] = "IDLE"
    /\ CASE exec_state = "BLOCKED" ->
            /\ exec_state' = "WAKING"
            /\ park_token' = "TOK_NONE"
            /\ sig_stage' = "SENT"
            /\ waker_stage' = [waker_stage EXCEPT !["w_sig"] = "IN_WAKING"]
         [] exec_state \in {"SW_PARK", "WAKING"} ->
            /\ park_token' = "TOK_NOTIFIED"
            /\ sig_stage' = "SENT"
            /\ waker_stage' = [waker_stage EXCEPT !["w_sig"] = "DONE"]
            /\ UNCHANGED exec_state
         [] exec_state \in {"RUNNING", "SW_PREEMPT", "READY"} ->
            /\ park_token' = IF park_token = "TOK_ARMED" THEN "TOK_NOTIFIED" ELSE park_token
            /\ sig_stage' = "SENT"
            /\ waker_stage' = [waker_stage EXCEPT !["w_sig"] = "DONE"]
            /\ UNCHANGED exec_state
         [] exec_state \in {"EXITING", "DEAD"} ->
            /\ sig_stage' = "SENT"
            /\ waker_stage' = [waker_stage EXCEPT !["w_sig"] = "DONE"]
            /\ UNCHANGED <<exec_state, park_token>>
    /\ UNCHANGED <<pc, cond, rq, cpu_curr, cpu_prev, cpu_prev_mode, cpu_prev_tok,
                   cpu_popped, post_stage, stack_users, slice_stepped, terminated>>

\* Concurrent Spurious / Timer Waker (`w_spur`): fires once without setting cond = 1,
\* racing directly against SignalWakeup, PostSwitch, and ExternalKill!
SpuriousWakeup ==
    /\ waker_stage["w_spur"] = "IDLE"
    /\ CASE exec_state = "BLOCKED" ->
            /\ exec_state' = "WAKING"
            /\ park_token' = "TOK_NONE"
            /\ waker_stage' = [waker_stage EXCEPT !["w_spur"] = "IN_WAKING"]
         [] exec_state \in {"SW_PARK", "WAKING"} ->
            /\ park_token' = "TOK_NOTIFIED"
            /\ waker_stage' = [waker_stage EXCEPT !["w_spur"] = "DONE"]
            /\ UNCHANGED exec_state
         [] exec_state \in {"RUNNING", "SW_PREEMPT", "READY"} ->
            /\ park_token' = IF park_token = "TOK_ARMED" THEN "TOK_NOTIFIED" ELSE park_token
            /\ waker_stage' = [waker_stage EXCEPT !["w_spur"] = "DONE"]
            /\ UNCHANGED exec_state
         [] OTHER ->
            /\ waker_stage' = [waker_stage EXCEPT !["w_spur"] = "DONE"]
            /\ UNCHANGED <<exec_state, park_token>>
    /\ UNCHANGED <<pc, cond, sig_stage, rq, cpu_curr, cpu_prev, cpu_prev_mode,
                   cpu_prev_tok, cpu_popped, post_stage, stack_users,
                   slice_stepped, terminated>>

\* Waker transitions WAKING -> READY (or pushes first if BugPushBeforeReady).
WakerFinishWaking(w, target_cpu) ==
    /\ waker_stage[w] = "IN_WAKING"
    /\ IF BugPushBeforeReady
       THEN \* DEFECT 2: Push to target_cpu's rq BEFORE setting exec_state := READY!
            /\ rq' = [rq EXCEPT ![target_cpu] = Append(@, "T")]
            /\ waker_stage' = [waker_stage EXCEPT ![w] = "NEED_READY"]
            /\ UNCHANGED <<exec_state, park_token>>
       ELSE IF BugWakingBlindStore
       THEN \* DEFECT 4: Blind store (READY, TOK_NONE), clobbering TOK_NOTIFIED and EXITING!
            /\ exec_state' = "READY"
            /\ park_token' = "TOK_NONE"
            /\ waker_stage' = [waker_stage EXCEPT ![w] = "NEED_PUSH"]
            /\ UNCHANGED rq
       ELSE \* CORRECT: CAS((WAKING, tok) -> (READY, tok))!
            IF exec_state = "WAKING"
            THEN /\ exec_state' = "READY"
                 /\ waker_stage' = [waker_stage EXCEPT ![w] = "NEED_PUSH"]
                 /\ UNCHANGED <<park_token, rq>>
            ELSE \* Task transitioned to EXITING concurrently -> abort push!
                 /\ waker_stage' = [waker_stage EXCEPT ![w] = "DONE"]
                 /\ UNCHANGED <<exec_state, park_token, rq>>
    /\ UNCHANGED <<pc, cond, sig_stage, cpu_curr, cpu_prev, cpu_prev_mode,
                   cpu_prev_tok, cpu_popped, post_stage, stack_users,
                   slice_stepped, terminated>>

WakerPushRq(w, target_cpu) ==
    /\ waker_stage[w] = "NEED_PUSH"
    /\ rq' = [rq EXCEPT ![target_cpu] = Append(@, "T")]
    /\ waker_stage' = [waker_stage EXCEPT ![w] = "DONE"]
    /\ UNCHANGED <<exec_state, park_token, pc, cond, sig_stage, cpu_curr, cpu_prev,
                   cpu_prev_mode, cpu_prev_tok, cpu_popped, post_stage,
                   stack_users, slice_stepped, terminated>>

WakerLateReady(w) ==
    /\ waker_stage[w] = "NEED_READY"
    /\ waker_stage' = [waker_stage EXCEPT ![w] = "DONE"]
    /\ IF exec_state = "WAKING"
       THEN exec_state' = "READY"
       ELSE UNCHANGED exec_state
    /\ UNCHANGED <<park_token, pc, cond, sig_stage, rq, cpu_curr, cpu_prev,
                   cpu_prev_mode, cpu_prev_tok, cpu_popped, post_stage,
                   stack_users, slice_stepped, terminated>>

-----------------------------------------------------------------------------
(* 5. Task Termination & Quiescent Reclamation (EXITING -> DEAD)           *)
-----------------------------------------------------------------------------
ExternalKill ==
    /\ AllowExternalKill
    /\ ~terminated
    /\ exec_state \notin {"EXITING", "DEAD"}
    /\ exec_state' = "EXITING"
    /\ park_token' = "TOK_NONE"
    /\ terminated' = TRUE
    /\ UNCHANGED <<pc, cond, sig_stage, rq, cpu_curr, cpu_prev, cpu_prev_mode,
                   cpu_prev_tok, cpu_popped, post_stage, waker_stage,
                   stack_users, slice_stepped>>

ReapDeadTask ==
    /\ exec_state = "EXITING"
    /\ stack_users = {}
    /\ CountInRq = 0
    /\ CountPopped = 0
    /\ CountEnqueuing = 0
    /\ \A c \in CPUs : cpu_curr[c] # "T" /\ cpu_prev[c] # "T" /\ post_stage[c] = "NONE"
    /\ \A w \in Wakers : waker_stage[w] \notin {"IN_WAKING", "NEED_PUSH", "NEED_READY"}
    /\ exec_state' = "DEAD"
    /\ UNCHANGED <<park_token, pc, cond, sig_stage, rq, cpu_curr, cpu_prev,
                   cpu_prev_mode, cpu_prev_tok, cpu_popped, post_stage,
                   waker_stage, stack_users, slice_stepped, terminated>>

\* Terminal stuttering step (microkernel CPUs run idle loop once task is DEAD or DONE)
IdleTick ==
    /\ (exec_state = "DEAD" \/ (pc = "PC_DONE" /\ ~AllowExternalKill))
    /\ UNCHANGED vars

-----------------------------------------------------------------------------
(* Complete Next-State Relation                                            *)
-----------------------------------------------------------------------------
SchedStep ==
    \/ \E c, src \in CPUs : DequeueTask(c, src)
    \/ \E c \in CPUs :
        \/ TryDispatchPopped(c)
        \/ TaskPreparePark(c)
        \/ TaskCheckCond(c)
        \/ TaskFinishWait(c)
        \/ HandleKilledRunning(c)
        \/ PostSwitchFinishStack(c)
        \/ PostSwitchPushRq(c)
        \/ PostSwitchLateReady(c)
    \/ \E c \in CPUs : TaskCommitPark(c)
    \/ \E w \in Wakers, target \in CPUs :
        \/ WakerFinishWaking(w, target)
        \/ WakerPushRq(w, target)
    \/ \E w \in Wakers : WakerLateReady(w)
    \/ SignalSetCond
    \/ SignalWakeup
    \/ ReapDeadTask

EnvInterferenceStep ==
    \/ \E c \in CPUs :
        \/ PreemptTask(c)
        \/ TaskCancelBeforeCommit(c)
        \/ TaskRunDoneWork(c)
    \/ SpuriousWakeup
    \/ ExternalKill
    \/ IdleTick

Next == SchedStep \/ EnvInterferenceStep

-----------------------------------------------------------------------------
(* Safety Invariants                                                       *)
-----------------------------------------------------------------------------
TypeOK ==
    /\ exec_state \in ExecStates
    /\ park_token \in ParkTokens
    /\ pc \in PCStates
    /\ cond \in {0, 1}
    /\ sig_stage \in {"INIT", "COND_SET", "SENT"}
    /\ stack_users \subseteq CPUs
    /\ slice_stepped \in BOOLEAN
    /\ terminated \in BOOLEAN

\* S1: At most one CPU executes T at any instant, and only when RUNNING or EXITING.
ExclusiveCpuExecution ==
    /\ Cardinality({c \in CPUs : cpu_curr[c] = "T"}) <= 1
    /\ \A c \in CPUs : cpu_curr[c] = "T" => exec_state \in {"RUNNING", "EXITING"}

\* S2: Kernel stack ownership never overlaps across two CPUs (prevents early-steal stack corruption).
StackOwnershipSafe ==
    /\ Cardinality(stack_users) <= 1
    /\ \A c \in CPUs : (cpu_curr[c] = "T" \/ cpu_prev[c] = "T") => c \in stack_users

\* S3: At most one scheduling handle (runqueue slot, popped local, or pending push) exists globally.
QueueHandleConservation ==
    (CountInRq + CountPopped + CountEnqueuing) <= 1

\* S4: Every task visible in any runqueue has already transitioned to READY (or EXITING after publication).
ReadyBeforePublished ==
    InAnyRq => exec_state \in {"READY", "EXITING"}

\* S5: No READY task is ever orphaned outside all runqueues and active enqueuers.
NoOrphanedReadyTask ==
    (exec_state = "READY") => (CountInRq + CountPopped + CountEnqueuing = 1)

\* S6: Once terminated (EXITING/DEAD), T never resurrects to a live scheduling state,
\*     and DEAD implies zero stack/runqueue/enqueuer references remain.
NoDeadRequeue ==
    /\ terminated => exec_state \in {"EXITING", "DEAD"}
    /\ (exec_state = "DEAD") => (stack_users = {} /\ CountInRq = 0 /\ CountPopped = 0 /\ CountEnqueuing = 0)

\* S7: Once signal wakeup has completed (cond = 1 and wakeup(T) finished), T never rests in BLOCKED.
NoMissedSignalBlock ==
    (sig_stage = "SENT") => (exec_state # "BLOCKED")

-----------------------------------------------------------------------------
(* Temporal Liveness Specification & Properties (Weak Fairness ONLY, NO SF)*)
-----------------------------------------------------------------------------
Fairness ==
    /\ WF_vars(SignalSetCond)
    /\ WF_vars(SignalWakeup)
    /\ WF_vars(ReapDeadTask)
    /\ \A c, src \in CPUs : WF_vars(DequeueTask(c, src))
    /\ \A c \in CPUs :
        /\ WF_vars(TryDispatchPopped(c))
        /\ WF_vars(TaskPreparePark(c))
        /\ WF_vars(TaskCheckCond(c))
        /\ WF_vars(TaskFinishWait(c))
        /\ WF_vars(TaskCommitPark(c))
        /\ WF_vars(HandleKilledRunning(c))
        /\ WF_vars(PostSwitchFinishStack(c))
        /\ WF_vars(PostSwitchPushRq(c))
        /\ WF_vars(PostSwitchLateReady(c))
    /\ \A w \in Wakers, target \in CPUs :
        /\ WF_vars(WakerFinishWaking(w, target))
        /\ WF_vars(WakerPushRq(w, target))
    /\ \A w \in Wakers : WF_vars(WakerLateReady(w))

SpecSafety   == Init /\ [][Next]_vars
SpecLiveness == Init /\ [][Next]_vars /\ Fairness

\* L1: Signal delivery eventually wakes T to completion (unless T is externally killed).
NoLostWakeup ==
    []((sig_stage = "SENT") => <>(pc = "PC_DONE" \/ terminated))

\* L2: Every terminated task eventually drains all references and reaches DEAD.
CleanTermination ==
    [](terminated => <>(exec_state = "DEAD"))

=============================================================================

