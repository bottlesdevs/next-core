# bottles-core

The application core for managing Bottles Next Wine and Proton environments.

`bottles-core` imports and installs managed components, persists bottles,
executes Windows programs through WineBridge, and provides Virgo storage and
snapshots through the default `fvs` feature. Standard creation, launch, and addon
changes work without FVS even when that feature is compiled in.

Disable FVS when only conventional, directly mutable prefixes are needed:

```toml
[dependencies]
bottles-core = { version = "0.1", default-features = false }
```

Without `fvs`, snapshot APIs and Virgo storage are not compiled. Standard addon
changes always use direct writes; failed or cancelled recipes can leave partial
prefix changes.

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

Execution settings live in `BottleState::environment()` as an `EnvironmentState`.
Use `Bottle::edit` for metadata, environment variables and wrappers. These settings
apply on the next startup. Stop the environment before explicit software changes. `Bottle::launch(group_id, LaunchSpec)` runs an unregistered
program; `Bottle::launch_program(uuid)` runs a saved registration. Dropping a
bottle handle leaves Wine running; call `stop()` to shut it down.

Choose a prefix backend when creating a bottle. Standard installs directly into
a conventional Wine prefix. Virgo is experimental: it combines shared immutable
layers with each bottle's private writable data, preparing missing layers at
startup. Virgo requires FVS and a downloaded Soda runner to build its base.

Fetch addons from the component and dependency catalogs, or use
`Addons::import_component` to import a component archive. Local releases are
identified by UUID. Removing a release deletes its installation inputs;
runtime executables and Standard recipe-based removal still require those files.

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
use bottles_core::{Bottles, Config, LaunchSpec, SearchSource};
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
        let program = LaunchSpec::new("Example", "C:/Games/example.exe")?;
        let id = bottle.edit(move |edit| Ok(edit.add_program(program))).await?;
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

Bottle configuration uses version 2 in `bottle.toml`. Version 1 records and
snapshots are incompatible and are left untouched; no migration is provided.
Registration UUIDs belong to the bottle's program map, not to `LaunchSpec`.
The prefix backend is stored on `BottleState` and is fixed at creation.

Bottles delegate state publication, persistence and execution to one internal
`Environment<T>`. The environment holds the coordination lock through asynchronous
cleanup and uses `WineBridgeClient` directly. Handle identity is read from the
published state and is unavailable after deletion.
