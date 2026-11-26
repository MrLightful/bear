This copy is based on the published `huggler 0.0.1` crate. Bear uses a Cargo
patch for the runtime operations that release and restore managed worktrees
while retaining their conversations. The release state lives in the
`bear_released_worktree` table so it can coexist with upstream schema
migrations.
