# bottles-core

`bottles-core` is the application library behind Bottles Next. It manages Wine
and Proton environments, installable runtime components, saved Windows launch
definitions, user profiles, and launchable library entries.

The main entry point is [`Bottles`]. Opening it discovers persisted bottles,
loads configured plugins, and exposes the managers used by the rest of the
application. Long-running changes return an [`Operation`]: a lazy future with
progress reporting and cooperative cancellation.

## Quick start

Launch settings can be built without opening local services:

```rust
use bottles_core::ProgramSpec;

let program = ProgramSpec::new("Notepad", r"C:\windows\notepad.exe")
    .with_args([r"C:\notes.txt"])
    .with_new_console(true);

assert_eq!(program.name(), "Notepad");
assert_eq!(program.args(), &[r"C:\notes.txt"]);
assert!(program.new_console());
```

A complete application opens the core once, keeps it alive while managers and
operations are in use, and shuts down its background download service before
exiting:

```rust,no_run
use std::sync::Arc;

use bottles_core::{Bottles, Config};
use bottles_plugin_host::Plugins;

# async fn run(plugins: Arc<Plugins>) -> Result<(), bottles_core::error::Error> {
let core = Bottles::open(Config::default(), plugins).await?;

for bottle in core.bottles().list() {
    println!("{}", bottle.state()?.name());
}

core.shutdown().await?;
# Ok(())
# }
```

## Core concepts

- [`Bottles`] owns the shared services and top-level managers.
- [`Manager`] provides the current collection of [`Bottle`] handles and, with
  the `fvs` feature, standalone `Program` handles.
- [`State`] values are immutable snapshots. Handle watchers publish later
  snapshots without mutating earlier ones.
- [`Edit`] collects a draft configuration change and publishes it only after
  validation and persistence succeed.
- [`Addons`] lists, downloads, imports, and removes Wine runners and other
  managed components.
- [`Library`] combines launchable entries from bottles, standalone programs,
  and plugins.
- [`Profiles`] stores application profiles and their linked external accounts.

## Operations and cancellation

An [`Operation`] does not start until it is polled. Await it directly, spawn it
on an executor, or call [`Operation::cancel`] and await the result. Dropping an
operation merely stops polling it; it does not guarantee cleanup or cancellation.
Progress is advisory and intermediate updates may be coalesced.

## Features

- `fvs` (enabled by default) adds Virgo layered-prefix storage, environment
  history and rollback, and standalone `Program` environments. It also starts
  or connects to an `fvs2d` process during [`Bottles::open`].

Disable default features when an application only needs conventional mutable
Wine prefixes:

```toml
[dependencies]
bottles-core = { version = "0.1", default-features = false }
```

## Minimum supported Rust version

This crate does not currently declare an MSRV. Use the current stable Rust
toolchain; future releases may adopt an explicit policy.

## License

Licensed under the GNU General Public License, version 3. See `LICENSE` in the
source distribution.
