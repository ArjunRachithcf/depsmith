"""Commit modified working-tree files to a branch through the GitHub API.

Usage: python scripts/commit-via-api.py OWNER/REPO BRANCH EXPECTED_HEAD MESSAGE

Commits created with GraphQL createCommitOnBranch are signed by GitHub, so
they satisfy signed-commit rules, and a GitHub App token makes them trigger
workflows. EXPECTED_HEAD makes the commit fail if the branch moved meanwhile.
Only modified files are sent; the token comes from GH_TOKEN. Prints the new
commit's SHA, or nothing when there is nothing to commit.
"""

import base64
import json
import os
import subprocess
import sys
import urllib.request
from pathlib import Path

MUTATION = """
mutation($input: CreateCommitOnBranchInput!) {
  createCommitOnBranch(input: $input) { commit { oid } }
}
"""


def modified_files() -> list[str]:
    output = subprocess.run(
        ["git", "diff", "--name-only", "--diff-filter=M", "HEAD"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return output.split()


def main() -> int:
    if len(sys.argv) != 5:
        print(__doc__, file=sys.stderr)
        return 2
    repository, branch, expected_head, message = sys.argv[1:]
    files = modified_files()
    if not files:
        return 0
    headline, _, body = message.partition("\n")
    variables = {
        "input": {
            "branch": {"repositoryNameWithOwner": repository, "branchName": branch},
            "expectedHeadOid": expected_head,
            "message": {"headline": headline, "body": body.strip()},
            "fileChanges": {
                "additions": [
                    {
                        "path": path,
                        "contents": base64.b64encode(Path(path).read_bytes()).decode(),
                    }
                    for path in files
                ]
            },
        }
    }
    request = urllib.request.Request(
        "https://api.github.com/graphql",
        data=json.dumps({"query": MUTATION, "variables": variables}).encode(),
        headers={
            "Authorization": f"Bearer {os.environ['GH_TOKEN']}",
            "Content-Type": "application/json",
        },
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        result = json.load(response)
    if result.get("errors"):
        print(json.dumps(result["errors"], indent=2), file=sys.stderr)
        return 1
    print(result["data"]["createCommitOnBranch"]["commit"]["oid"])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
