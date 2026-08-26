# AppWorld decision (AGE-24)

**Decision:** M4 Stage B does **not** use AppWorld for the first reproduction.

**Use instead:** FiNER (entity exact-match) plus a small synthetic tool-use environment that
exercises ACE's Generator / Reflector / Curator loop without AppWorld's full simulated
app stack.

**Why:** AppWorld is the single largest engineering cost in the eval stack (Python
subprocess protocol on top of `chatty-core`'s sandbox, TGC/SGC scoring, task loader). The
Master Research Plan already flags the ACE ablation ladder at n=40 as unmeasurable; cutting
AppWorld avoids budgeting weeks of bridge work before the mechanism is calibrated.

**Claim impact:** Downgrade M4 Stage B from "AppWorld +17 headline" to "FiNER directional
gain + synthetic tool-use playbook growth". The AppWorld number stays cited-not-reproduced
(same class as DGM SWE-bench and GEPA-vs-GRPO).

**Revisit when:** Stage 0 ACE calibration passes and FiNER Stage B is green — then
re-estimate an AppWorld bridge as a follow-up issue, not as a silent substitute.
