"""A stand-in for Atlassian's MCP server (stdio, newline-delimited JSON-RPC),
for dcode-ai's Jira sync tests. Same tool names; an in-memory project SHOP."""
import json
import sys

issues = [{"key": "SHOP-7", "summary": "Fix checkout", "status": "In Progress", "cat": "indeterminate"}]
counter = [100]
TOOLS = ["getAccessibleAtlassianResources", "searchJiraIssuesUsingJql", "createJiraIssue",
         "getTransitionsForJiraIssue", "transitionJiraIssue"]


def text(obj):
    return {"content": [{"type": "text", "text": json.dumps(obj)}]}


def call(name, a):
    if name == "getAccessibleAtlassianResources":
        return text([{"id": "cloud-1", "url": "https://acme.atlassian.net", "name": "acme"}])
    if name == "searchJiraIssuesUsingJql":
        return text({"issues": [{"key": i["key"], "fields": {
            "summary": i["summary"],
            "status": {"name": i["status"], "statusCategory": {"key": i["cat"]}},
            "priority": {"name": "Medium"}}} for i in issues]})
    if name == "createJiraIssue":
        counter[0] += 1
        key = "%s-%d" % (a["projectKey"], counter[0])
        issues.append({"key": key, "summary": a["summary"], "status": "To Do", "cat": "new"})
        return text({"id": "1", "key": key})
    if name == "getTransitionsForJiraIssue":
        return text({"transitions": [
            {"id": "21", "to": {"name": "In Progress", "statusCategory": {"key": "indeterminate"}}},
            {"id": "41", "to": {"name": "Done", "statusCategory": {"key": "done"}}}]})
    if name == "transitionJiraIssue":
        for i in issues:
            if i["key"] == a["issueIdOrKey"]:
                i["status"], i["cat"] = ("Done", "done") if a["transitionId"] == "41" else ("In Progress", "indeterminate")
        return {"content": [{"type": "text", "text": "ok"}]}
    return {"content": [{"type": "text", "text": "unknown tool"}], "isError": True}


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    if "id" not in msg:
        continue
    m = msg.get("method")
    if m == "initialize":
        result = {"protocolVersion": "2025-03-26", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "fake-jira", "version": "1"}}
    elif m == "tools/list":
        result = {"tools": [{"name": t, "inputSchema": {"type": "object"}} for t in TOOLS]}
    elif m == "tools/call":
        p = msg.get("params", {})
        result = call(p.get("name"), p.get("arguments", {}))
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": result}) + "\n")
    sys.stdout.flush()
