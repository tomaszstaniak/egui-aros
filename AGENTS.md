# Repository instructions

Identify the containing product workspace and the actual target repository. Read
applicable ancestor AGENTS.md and referenced CLAUDE.md files, then repository and
subdirectory instructions. Do not assume the host loads these automatically.

Find the nearest .workspace.json from the target directory. Resolve paths relative
to its containing directory. Read standard.json, package README, core and selected
profiles, and the recorded participation decision. A configured proposal is not
adoption; preserve already authorized baselines and scope. Shared standards do not
automatically override ancestors or explicit user decisions. For AROS, read the
configured resources.aros instructions before platform choices; preserve the selected
mainline v1 / ABIv11 priority and verify each target independently.

Read this repository's README.md and docs/README.md: these are public, tracked files.
Private project documentation lives in the containing workspace's docs/, outside
this repository. Resolve adaptations relative to this repository's project.json;
read the private documentation map, directory map and affected current-state when
available. Do not stage, copy or publish private backlog, research, reports or evidence.
Public files must work in a standalone clone without private docs or sibling repos.

A standalone clone may lack the containing workspace. Read public instructions;
report missing private context honestly and pause only work that requires it. Do not
create parent documents automatically or invent missing adoption/history. A fresh
scaffold does not recover prior decisions. Protect private documents with backups.

Consult repository boundaries before editing siblings: owned component repositories
are independent targets, and references/ holds study material, not implicit edit or
build dependencies. Worktrees represent branches, not new products; identify which
branch a current-state account describes. Preserve local files and Git history.


Before cloning or editing, classify each repository in the private directory map.
Study-only external clones belong in outer references/. External sources we intend
to modify use upstream-patches inside our named project repository: upstreams.json,
clean upstream/<id>/, editable work/<id>/ and versioned patches/<id>/series.
A local clone, fork or planned upstream PR does not waive this workflow. Git commits
in work/ do not replace the patch series. Apply ancestor rules and record only
explicitly authorized scoped exceptions. See the creation guide's required role selection.

## Local commands

Public contributor commands belong in docs/development.md. Private run context and
results belong in the workspace docs/. Record scoped adaptations in the configured
private document. Read subdirectory instructions before changing their contents.

Save and version every external-source patch in our owned repository, including
locally applied changes and especially changes intended for upstream commits/PRs.
Commit the patch, metadata and series before submission; work-copy commits and
upstream submissions do not replace this record. Record actual verification and
upstream status/links, and preserve accepted patches in history when retiring them.
