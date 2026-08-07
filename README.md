# next-core

`bottles-core`, the core library implementing Bottles Next's bottle and
prefix management logic. Consumed directly by [`next-cli`](../next-cli) and
[`next-ui`](../next-ui) as an in-process Rust dependency — there is currently
no stable network/IPC API (see [`next-server`](../next-server)).

## Key types

- `Core` / `CoreBuilder` — the library's entry point; built with a
  `Paths`, a `DownloadManager`, and optional `fvs2d` executable + component/dependency
  catalog URLs.
- `BottleManager` / `Bottle` — create, list, and manage bottles: programs,
  environment variables, DLL overrides, snapshots, wrappers (Gamescope,
  MangoHud), and processes.
- `Library` — loads and refreshes the component (runners) and dependency
  (addons) catalogs, and drives downloads/installs.
- `RunnerKind` — Wine or Proton runner selection.
- `Environment` / `Paths` — filesystem layout and environment variable helpers.

## WineBridge

Each running bottle communicates with a `bottles-winebridge` agent spawned
inside its Wine prefix over gRPC (see `src/winebridge.rs`), used for process
management, the Windows registry, services, DLL overrides, and the filesystem
inside the prefix.

## Storage backends

Bottles can use either standard directory-based storage or `fvs2d`
(FUSE-based, snapshot/restore-capable) storage, see `src/prefix`.
