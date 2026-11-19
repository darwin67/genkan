# StatusMessage

Prompt text that does not belong in the placeholder appears above the pill. PAM notices and application status go below it, including authentication progress and “Press Return to try again” on lock after a failed attempt. The lock retry hint does not depend on PAM sending a notice.

Message text uses `type-message` with a soft shadow and wraps at `message-width`.

**Provide.** The message and whether it is a prompt (above) or a notice or status (below).
