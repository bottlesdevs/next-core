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
- `Library` projects registered programs across bottles and provides one-shot
  local search.
- `Profiles` persists named application identities and the current selection.
- `Operation<T>` represents long-running work with progress and cooperative
  cancellation.

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
use bottles_core::{Bottles, Config, Program, SearchAction};
use futures_lite::StreamExt;

#[tokio::main]
async fn main() -> Result<(), bottles_core::error::Error> {
    let bottles = Bottles::open(Config::default()).await?;

    if bottles.profiles().selected().is_none() {
        let profile = bottles.profiles().create("Player").await?;
        bottles.profiles().select(profile.id()).await?;
    }

    for bottle in bottles.bottles().list() {
        let state = bottle.state()?;
        println!("{}\t{}", state.id(), state.name());
    }

    if let Some(bottle) = bottles.bottles().list().into_iter().next() {
        let program = Program::new("Example", "C:/Games/example.exe")?;
        let mut edit = bottle.edit();
        edit.add_program(program.clone());
        edit.commit().await?;
        println!("registered {} as {}", program.name(), program.id());
    }

    let mut installed = Box::pin(bottles.library().watch());
    println!("{} installed programs", installed.next().await.unwrap().len());

    let mut search = Box::pin(bottles.library().search("example"));
    while let Some(entry) = search.next().await {
        println!("{}", entry.title());
        for action in entry.actions() {
            if let SearchAction::Launch(item) = action {
                println!("  launch {}", item.program()?.name());
            }
        }
    }

    bottles.close().await
}
```

## Getting help

Build the API documentation locally with `cargo doc -p bottles-core --open`.
Report bugs through the [issue tracker].

## License

Licensed under the [GNU General Public License, version 3](LICENSE).

[Source]: https://github.com/bottlesdevs/next-core
[Issue tracker]: https://github.com/bottlesdevs/next-core/issues
