## Test Case: Agent Session Creation

**Scenario:** A user starts a new session via the CLI.

**Steps:**
1. Run `dcode-ai` without arguments
2. Enter a prompt: "Hello, agent!"
3. End with: "what is your problem"

**Expected result:** A new session is created under `.dcode-ai/sessions/` with a unique ID, and the response is streamed back to the terminal.
