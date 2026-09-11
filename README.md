# bottles-core

The application core for managing Bottles Next Wine and Proton environments.

`bottles-core` discovers and installs managed components, persists bottles,
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
prefix changes. Explicit Standard snapshots initialize FVS history on demand.

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

Execution settings live in `BottleState::environment()` as an `EnvironmentConfig`.
Cloned bottle handles share an operation mutex. Each runtime call attaches through
a temporary environment connection; registered programs use the bottle's settings. `Bottle::launch(ProgramSpec)` runs an
unregistered executable; `Bottle::launch_program(uuid)` runs a registration.
Both return `Operation<u32>` with the initial Windows process ID.

Process inspection and group kill attach to an existing runtime without starting
Wine or inspecting FVS mounts. Startup still checks existing Virgo mounts against
saved layers and the private upper when preparing storage. Dropping handles leaves
Wine running. Explicit `stop()` waits
for wineserver before unmounting, even when WineBridge cannot be reached.
Unreachable discovery and mismatched mounts require `stop()` before retrying.

Initialization and recipes run without game wrappers. Cancellation finishes cleanup
before returning; failed shutdown retains storage. Temporary cache failures can
require manual cleanup of the reported prefix. Automatic recovery and concurrent
independent clients are deferred.

`Bottle::edit(|state| { /* changes */ Ok(()) })` returns an `Operation<()>`.
The callback receives a draft of the latest state under the owner lock. Edit
`name`, `programs`, and `environment` directly; errors discard the whole draft.
Metadata edits work while running. Call `stop()` before changing environment
settings, including through `set_component`, `remove_component`, or `install`.
Storage is fixed at creation; existing dependencies remain in installation order.
New dependency selections can be appended. Prefix changes run before publication;
batch rollback of those effects remains part of the later composition work.
Settings-only edits save the draft without preparing or mutating the prefix.

Virgo builds a clean shared base from the latest catalog runner named `Soda`
(case-insensitive, semantic-version ordering). That exact release must already be
downloaded. The base manifest pins its release and immutable FVS revision across
catalog refreshes. `Bottles::rebuild_virgo_base()` explicitly publishes a new base;
stopped preparations adopt it, while running environments and snapshots retain
their resolved revisions. Older base generations and legacy caches are retained.

Addon cache misses use pinned Soda and declared prerequisite layers, without
owner settings, wrappers, or private writable data. UUID remains the sole cache
identity: completed caches survive runner changes and base rebuilds. Runner
adapters are built using the selected runner over each base generation. Shared
construction is serialized within one core instance. Standard installers continue
using their owner's runner. Managed registry-baseline composition remains step 8;
Virgo retains its existing per-addon checkpoint boundary.

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
