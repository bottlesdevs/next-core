# bottles-core

`bottles-core` is the application library behind Bottles Next. It manages Wine
and Proton prefixes, runtime tools and prefix components, and launches
Windows programs in them.

The main entry point is [`Core`]. Opening it resolves the application's data
directories, loads persisted state, initializes addon and prefix
registries. Long-running changes
return an [`Operation`]: a lazy future with progress reporting and cooperative
cancellation.

## Quick start

A complete application opens the core once, keeps it alive while managers and
operations are in use, and shuts down its background download service before
exiting. Opening performs filesystem I/O and, with `fvs` enabled,
may start an `fvs2d` process:

```rust,no_run
use bottles_core::Core;

# async fn run() -> Result<(), bottles_core::error::Error> {
let core = Core::builder().build().await?;

for prefix in core.prefixes().list() {
    println!("{}: {:?}", prefix.id(), prefix.state()?.backend);
}

core.shutdown().await?;
# Ok(())
# }
```

## Core concepts

- [`Core`] owns the shared services and top-level managers.
- [`Prefixes`] provides the current collection of [`Prefix`] handles and
  publishes every prefix's state as one [`Inventory`].
- [`PrefixState`] values are immutable snapshots. Handle watchers publish
  later snapshots without mutating earlier ones.
- [`Edit`] collects a draft configuration change and publishes it only after
  validation and persistence succeed.
- [`Addons`] downloads, imports, and removes Wine runners and other managed
  components. [`AddonsState`] provides immutable catalog and release snapshots
  through [`Addons::state`] and [`Addons::watch`].

## Operations and cancellation

An [`Operation`] does not start until it is polled. Await it directly, spawn it
on an executor, or call [`Operation::cancel`] and await the result. Dropping an
operation merely stops polling it; it does not guarantee cleanup or cancellation.
Progress is advisory and intermediate updates may be coalesced.

Call [`Core::shutdown`] only after application work has stopped. Shutdown
cancels queued and in-flight downloads, waits for download workers to exit, and
prevents later download-backed operations from starting successfully. It does
not stop Wine processes; stop each running [`Prefix`] explicitly. With
`fvs` enabled, the connected or spawned `fvs2d` daemon also remains running for
later clients.

## Async runtime

The crate does not start or require a particular executor, with one
exception: talking to a prefix's `WineBridge` goes through tonic's transport,
which needs a Tokio runtime. Poll [`Prefix`] operations, and prefix creation,
edits, and deletion, which may stop a running `WineBridge`, from within a Tokio
runtime.

## Features

- `fvs` (enabled by default) adds Virgo layered-prefix storage and prefix
  history and rollback. It also starts or connects to an `fvs2d` process during
  [`CoreBuilder::build`]. Set
  `CoreBuilder::fvs2d` to an explicit executable path or leave it unset to resolve
  `fvs2d` through `PATH`.

Disable default features when an application only needs conventional mutable
Wine prefixes:

```toml
[dependencies]
bottles-core = { version = "0.1", default-features = false }
```

## Minimum supported Rust version

This crate does not declare an MSRV.

## License

Licensed under the GNU General Public License, version 3. See `LICENSE` in the
source distribution.
