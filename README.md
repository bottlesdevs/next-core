# bottles-core

The application core for managing Bottles Next Wine and Proton environments.

`bottles-core` imports and installs managed components, persists bottles,
executes Windows programs through WineBridge, and provides Virgo storage and
snapshots through the default `fvs` feature. With this feature enabled,
`Bottles::open` connects to or starts FVS once and fails if initialization fails.
`Config::fvs2d` supplies an executable path; when omitted, `fvs2d` is found through
PATH. The socket remains in the configured runtime directory.

Disable FVS when only conventional, directly mutable prefixes are needed:

```toml
[dependencies]
bottles-core = { version = "0.1", default-features = false }
```

Without `fvs`, snapshot APIs, Virgo storage, and standalone programs are not
compiled. Standard addon changes always use direct writes; failed or cancelled
recipes can leave partial prefix changes. A software edit shares one maintenance
session across its removals and installations, stopping once afterward even on
failure or cancellation. Payloads are accessed as steps run, so missing or invalid
inputs can fail after earlier steps have changed the prefix.
Runtime-only selection changes do not start that session.

[Source] | [Issue tracker]

## Overview

The crate is centered around six types:

- `Bottles` owns the download service and provides addon, bottle, and standalone
  program managers.
- `Addons` publishes live collections of runners and installable addons. Item
  values are snapshots; query the manager again after a publication.
- `BottleManager` manages bottles by UUID. A `Bottle` is a shared handle whose
  current immutable `BottleState` can be read or watched.
- `Library` combines bottle registrations and standalone programs, searching
  them alongside games owned through the selected profile's storefront plugins.
- `Profiles` persists named application identities and the current selection.
- `Operation<T>` represents long-running work with progress and cooperative
  cancellation.

Execution settings live in `BottleState::environment()` as an `EnvironmentState`.
Use `Bottle::edit` or `Program::edit` to change metadata, startup settings, and
software selections in one draft. `Edit::set_component`, `remove_component`, and
`add_dependency` change only that draft; the final selection is validated and
applied before saving and publishing once. Dependencies remain append-only.
Software changes require a stopped owner. Metadata, environment variables, and
wrappers can change while running; startup settings apply on the next launch.
`Bottle::launch(group_id, ProgramSpec)` runs an unregistered program;
`Bottle::launch_program(uuid)` runs a saved registration. Dropping a bottle handle
leaves Wine running; call `stop()` to shut it down.

Choose a prefix backend when creating a bottle. Standard installs directly into
a conventional Wine prefix. Virgo is experimental: it combines shared immutable
layers with each bottle's private writable data. Missing layers are prepared
before publishing owner selections. Creation and software edits compose the
owner's registry once for the final selection. Launch mounts that prepared
storage without registry recomposition or an automatic history checkpoint.
The initial base uses the greatest valid local Soda version and remains pinned.
Cached artifacts can be reused without their shared source payloads.

Fetch addons from the component and dependency catalogs, or use
`Addons::import_component` to import a component archive. Local releases are
identified by UUID. Removing a release deletes its installation inputs;
runtime executables and new installations still require those files. Standard
removal uses saved recipes and existing backups.

Operations are lazy. Await them, call `cancel().await`, or spawn them and
explicitly detach the task; dropping an operation abandons it.

## Internal Wine layer storage

`Context` owns the ready `Arc<Fvs2dClient>`. The shared `VirgoManager` owns a
`LayerStore` with `Directories`, a clone of that client, and one construction
and publication lock. The store has no Context, addon, runner, or owner knowledge.
Virgo policy chooses Soda and build-time WineBridge, supplies relative addresses
and layer order, and executes recipes through the existing runners.

The store owns cache lookup, staging, filesystem and registry capture, immutable
publication, composition, and workspace mounts. Bases retain their initial hives;
overlays store registry patches separately from filesystem effects. Composition
uses the supplied overlay order, then replays private registry changes.
Artifact paths and manifest version 1 are unchanged. Virgo edits release stopped
owner storage before checkpointing, then compose registry changes and save the
draft together. Composition or save failures restore the checkpoint before
returning; nothing is published on failure. Shared builds retain their separate
scratch prefixes and shutdowns. Owner edits need no Wine startup or mount.

Virgo owner directories and snapshots created under the earlier launch-time
composition lifecycle must be recreated. Schemas remain at version 1; there is
no migration, launch fallback, or automatic deletion. Existing shared layers
remain reusable.

`get_or_build` holds coordination through the complete cache-miss workflow.
Policy resolves inputs, prepares storage, executes work, and stops Wine before
passing the execution result to finalization. Finalization publishes successful
work or cleans up failed work. Failed shutdown, mount creation, or unmount retains
staging; dropping a workspace never performs asynchronous cleanup. Environment
owns edit checkpoints, discovery cleanup and configuration publication.
Snapshots include the prepared registry baseline, private data and saved
selections; restoration does not rebuild shared layers.

Internal listing reads immediate artifacts in a supplied collection. Removal
withdraws an explicit address into trash under the publication lock before
best-effort cleanup. Its caller must ensure the artifact is unmounted and no
longer needed. There is no owner tracking, garbage collection, or public layer API.

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
