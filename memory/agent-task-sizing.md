---
name: agent-task-sizing
description: User wants background agent tasks scoped small (~under 30 min); split large/open-ended work into stages.
metadata:
  type: feedback
---

The user dislikes background agents that run 30+ minutes. Scope each agent so it finishes well under that.

**Why:** Long single-shot agents are hard to monitor, can spiral on open-ended work (e.g. the NV2A rendering agent), and tie up feedback for too long. Smaller agents return sooner, are reviewable, and let the user redirect.

**How to apply:** For hard/open-ended tasks, split into stages and run them sequentially: first a *diagnose/trace only* agent that reports findings (no fix), then — after reviewing — a focused *implement-the-specific-fix* agent. Prefer several short agents over one big one. The earlier diagnostic→fix split (HDD reboot, warm-boot marker) worked well; the all-in-one NV2A agent (trace + implement scanout) ran too long. Relates to [[gamecube-core]] and other core bring-up work.
