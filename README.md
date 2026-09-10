# bottles-core

The application core for managing Bottles Next Wine and Proton environments.

`bottles-core` discovers and installs managed components, persists bottles,
executes Windows programs through WineBridge, and provides checkpointed prefix
mutation and snapshots through the default `fvs` feature.

Disable FVS when only conventional, directly mutable prefixes are needed:

```toml
[dependencies]
bottles-core = { version = "0.1", default-features = false }
```

Without `fvs`, snapshot APIs and Virgo storage are not compiled, and failed or
cancelled addon recipes are not rolled back automatically.

[Source] | [Issue tracker]

## Overview

The crate is centered around six types:

- `Bottles` owns the download service and provides the addon and bottle managers.
- `Addons` publishes live collections of runners and installable addons. Item
  values are snapshots; query the manager again after a publication.
- `BottleManager` interns bottles by UUID. A `Bottle` is a shared handle whose
  current immutable `BottleState` can be read or watched.
- `Library` projects registered programs across bottles and searches them
  alongside games owned through the selected profile's storefront plugins.
- `Profiles` persists named application identities and the current selection.
- `Operation<T>` represents long-running work with progress and cooperative
  cancellation.

Execution settings live in `BottleState::environment()` as an
`EnvironmentConfig`. Each bottle retains a private environment on first runtime
use; clones share it. Creating the private environment resolves the runner,
prepares the prefix, and connects or starts WineBridge. Attach-only operations
connect without starting Wine or mounting storage. Its runner and bridge are
always present; configuration remains in the owner. Registered programs
use the bottle's settings. Use
`Bottle::launch(ProgramSpec)` to run an unregistered executable and
`Bottle::launch_program(uuid)` to run a registration. Both return `Operation<u32>`
with the initial Windows process ID. Dropping the bottle only detaches; call
`stop()` to stop its runtime. Shutdown uses the saved runner and storage settings,
with or without a cached environment. It requests bridge shutdown when reachable,
then terminates and waits for wineserver before unmounting. Bridge discovery or
shutdown failure does not skip wineserver shutdown; the cached environment is
cleared only after shutdown and unmounting succeed.

`Bottle::edit(|state| { /* changes */ Ok(()) })` returns an `Operation<()>`.
The callback receives a draft of the latest state under the owner lock. Edit
`name`, `programs`, and `environment` directly; errors discard the whole draft.
Metadata edits work while running. Call `stop()` before changing environment
settings, including through `set_component`, `remove_component`, or `install`.
Storage is fixed at creation; existing dependencies remain in installation order.
New dependency selections can be appended. Prefix changes run before publication;
batch rollback of those effects remains part of the later composition work.

Bottle configuration requires execution settings under `environment`, with
resolved FVS layers retained inside `environment.storage` for Virgo. Old
configurations are rejected during deserialization and left untouched; recreate
those bottles to use the new format. Completed addon caches remain reusable.

Operations are lazy. Await them, call `cancel().await`, or spawn them and
explicitly detach the task; dropping an operation abandons it.

## Example

Add `bottles-core` and an async runtime to your application:

```toml
[dependencies]
bottles-core = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
futures-lite = "2"
```

Open the library, inspect the current bottles, and stop its download service:

```rust
use bottles_core::{Bottles, Config, ProgramSpec, SearchSource};
use futures_lite::StreamExt;

#[tokio::main]
async fn main() -> Result<(), bottles_core::error::Error> {
    let bottles = Bottles::open(Config::default()).await?;

    println!("profile: {}", bottles.profiles().selected().name());

    for bottle in bottles.bottles().list() {
        let state = bottle.state()?;
        println!("{}\t{}", state.id(), state.name());
    }

    if let Some(bottle) = bottles.bottles().list().into_iter().next() {
        let program = ProgramSpec::new("Example", "C:/Games/example.exe")?;
        let id = program.id();
        bottle.edit(move |state| {
            state.programs.insert(program.id(), program);
            Ok(())
        }).await?;
        println!("registered {id}");
    }

    let mut installed = Box::pin(bottles.library().watch());
    println!("{} installed programs", installed.next().await.unwrap().len());

    let mut results = Box::pin(bottles.library().search("example"));
    while let Some(entry) = results.next().await {
        println!("{} ({})", entry.title(), entry.source_name());
        match entry.source() {
            SearchSource::Installed(item) => {
                println!("  launch {}", item.program()?.name());
            }
            SearchSource::Storefront { provider_id, .. } => {
                println!("  owned through {provider_id}");
            }
            _ => {}
        }
    }

    bottles.shutdown().await
}
```

## Getting help

Build the API documentation locally with `cargo doc -p bottles-core --open`.
Report bugs through the [issue tracker].

## License

Licensed under the [GNU General Public License, version 3](LICENSE).

[Source]: https://github.com/bottlesdevs/next-core
[Issue tracker]: https://github.com/bottlesdevs/next-core/issues
