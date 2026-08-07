# Permissions

Permissions are a product feature in Commonspace, not a compliance layer. The
goal: a nontechnical user always understands what the agent may touch, what it
wants to do next, and how to say no — and the enforcement is deterministic
Rust, never the model's judgment.

## Model

Every operation the agent performs through Commonspace tools is classified and
evaluated by `commonspace-permissions`:

```
PolicyInput {
  workspace_id,
  operation_class,   // see table
  canonical_paths,   // fully resolved, symlinks followed
  origin,            // which tool / provider session asked
}
→ Allow | RequireApproval { reason } | Deny { reason }
```

Every evaluation and every user decision is journaled to
`permission_decisions` and visible in the audit history.

## Default policy

| Operation | Default |
|---|---|
| Read explicitly selected files | Allow |
| Read within explicitly authorized folders | Allow (scope-checked) |
| Create files in an authorized workspace | Allow |
| Modify existing files | Preview or approval, per user setting |
| Rename / move within one folder, single file | Allow with journal + undo |
| Batch rename, cross-folder move | RequireApproval |
| Delete files | RequireApproval, always; goes to OS trash |
| Permanent deletion | Deny (disabled by default) |
| Run executables / installers | RequireApproval, always |
| Install packages | RequireApproval |
| Access paths outside the workspace | RequireApproval via native folder picker (grants a new scope) |
| Upload files | RequireApproval unless pre-authorized by a specific workflow |
| Send messages / email | RequireApproval, always, final confirmation |
| Publish / purchase / submit forms / external changes | RequireApproval, always, final confirmation |
| Read credentials or secrets | Deny |
| Protected OS directories (system roots, other users' data, credential stores) | Deny, not configurable |

Approvals are remembered at the narrowest sensible scope ("this file, this
task"), with explicit wider grants ("this folder, this workspace") available
but never the default.

## Path safety rules

- Canonicalize before evaluate. Windows quirks are normalized (`\\?\`
  prefixes, short names, drive-relative forms); reserved device names are
  rejected; alternate data streams are rejected for writes.
- A symlink or junction is only traversable if its *resolved target* is inside
  an authorized root.
- `..` components cannot escape a root because evaluation happens on resolved
  absolute paths, not on the requested string.
- Paths shown to the user in approval dialogs are the resolved destination
  paths, not the agent's requested strings.

## Provider permission bridging

Provider CLIs have their own permission systems. Adapters configure each CLI
so its permission prompts surface inside Commonspace's UI as
`permission.requested` events, and the user's answer is returned through the
provider's supported mechanism. Commonspace's engine remains the final
authority for anything executed through Commonspace's own tools; the
provider's sandbox/approval flags are set to their most restrictive suitable
level for everything else. Per-provider details: docs/provider-adapters.md.

## Approval UX requirements

- Dialogs state the operation in plain language, show resolved paths and
  counts ("Rename 14 files in Documents/Contracts"), and mark irreversible
  actions with an explicit warning.
- Batch operations name the individual items they affect, so "Rename 14 files
  in Documents/Contracts" can be read as fourteen resolved paths rather than
  taken on trust as a count. Approving a *subset* is not built: the answer to
  a batch is approve all or deny all. Per-item deselection is wanted, not
  shipped, and nothing in the UI should suggest otherwise.
- Denying is always safe: the task pauses or continues without the operation;
  it never errors into an unrecoverable state.
- Dialogs are keyboard-navigable and screen-reader labelled; approval is never
  conveyed by color alone.
