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

DLL override queries and changes also return `Operation<T>`, exposing preparation
progress and cooperative cancellation. Existing `.await` calls continue to work.
An `Environment` represents a running execution environment and holds a required
WineBridge connection. `attach_or_start` constructs it directly from the owner's
configuration and location, including after an application restart. The handle retains
neither configuration nor services; dropping it leaves Wine running. Launch and DLL
operations hold the owner lock and forward progress and cancellation. Registered
launch resolves its program before startup; other calls use `with_environment`.

Initialization, configuration edits, and shutdown are associated functions on
`Environment` that take the owner's inputs and return completion, without requiring
a running handle.
The owner retains configuration, persistence, publication, and coordination.
`PrefixBackend` owns how a runnable prefix is created and maintained: initialization, supported edits, software
materialization, composition, and storage release. Standard and Virgo implementations
live below this boundary; environment workflows do not distinguish between them.
Backends stop their initialization and installer processes through shared runtime
helpers, without invoking owner lifecycle operations.

Process inspection and group kill attach to an existing runtime without starting
Wine or inspecting FVS mounts. Startup still checks existing Virgo mounts against
resolved layers and the private upper when preparing storage. Dropping handles leaves
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
The prefix backend is fixed at creation. Standard dependencies may be appended; Virgo
selections may also be removed or reordered. Standard changes execute installers
before saving and keep direct-write semantics. Virgo creation and edits save
selections without building layers or changing private prefix data. Preparation
errors surface when starting the environment.

Local releases live at `components/releases/<uuid>/` or
`dependencies/releases/<uuid>/`. Both `release.toml` and `payload/` are required;
they are published and removed together. `Release<K>` is persisted directly and owns the local resource paths and frozen recipes.
The manager keeps separate typed component and dependency maps, with UUID uniqueness
checked across both families.
Each ordered resource keeps a path relative to `payload/` and its recipe.
Components use the payload directory itself; dependency resources use local filenames.
Download URLs and checksums remain in the catalog and are used only during fetching.
`Addon<K>` preserves identity, requirements, and frozen runtime variables in owner
state. A local release exists only as a complete record and
payload. Loading rejects incomplete releases. Removal renames the entire release
directory out of its published location, removes it from the typed map, then deletes
the withdrawn directory. A deletion failure leaves only unpublished staging data.
Existing environments keep their runtime variables after removal. Source installation
and runtime executables still require their payloads.
Fetching a removed release resolves it from the current catalog; imported components
must be imported again with a new UUID. Built Virgo caches are not removed.

Omitted component `steps` use the bundled recipe for that slot.
Explicit `steps` replace the default entirely; `steps: []` means no installation steps.
The resolved recipe is frozen into the local release, so template updates do not
change existing releases. Dependency steps come from the catalog (omitted means empty),
and both families take their requirements from the catalog.
Changed contents or recipes require a new UUID. Catalog refresh
only replaces catalog snapshots; it neither changes releases nor discovers files.

`import_component(path, slot, name, version)` extracts a local `.tar`, `.tar.gz`/`.tgz`,
or `.tar.xz`/`.txz` archive containing one top-level component directory. It assigns a
fresh UUID and freezes a bundled recipe into the release. Folder imports are not
supported. Imports work offline and leave the source archive untouched. Executable
permissions and internal relative symlinks are preserved; escaping links are rejected.
Catalog component downloads and local imports share archive preparation and release
publication; catalog downloads additionally transfer and verify the archive. Templates are never read during installation
or startup. Publication and loading check recognized runner layouts, WineBridge/UMU
entrypoints, and recipe Copy sources before making a component selectable.
Old indexes, slot/version directories, and the former top-level `releases/` directory
are ignored and left untouched;
re-download or explicitly import a component archive. There is no automatic migration.

Virgo builds a clean shared base from the latest catalog runner named `Soda`
(case-insensitive, semantic-version ordering). That exact release must already be
downloaded. The base manifest pins its release and immutable FVS revision across
catalog refreshes. The base is initialized once and reused; there is no rebuild
operation. Stopped preparation builds missing artifacts and resolves the selected
composition before execution. New bases and adapters use
`virgo/soda`; existing manifest references and addon caches remain usable.

Every addon recipe must install against pinned Soda alone. Cache construction
uses no layers, registry patches, or environment contributions from other addons,
and no owner settings, wrappers, or private writable data. Requirements are validated against
the final environment selections. UUID remains the sole cache
identity: completed caches survive runner and settings changes. Runner
adapters are built using the selected runner over the pinned base. Shared
construction is serialized within one core instance. Standard installers continue
using their owner's runner.

Virgo composition is always Soda base → selected runner adapter → components in
slot order → dependencies in persisted order → private writable upper. Preparation
resolves each selected addon's immutable layer by UUID and keeps the resulting
stack local to that operation. There is no second persisted list of selections or
layers. The shared base remains pinned and completed UUID caches remain immutable.

Each preparation reconstructs the managed registry baseline and reapplies private
changes relative to the previous baseline. The owner is checkpointed before that
mutation; failure restores its prior data while keeping the selected configuration
saved for retry. Shared artifact builds happen before the checkpoint. Private
files, updates, saves, and whiteouts keep normal overlay precedence; the upper is
never pruned. A later WineBridge startup failure does not undo successful registry
preparation.

Runtime variables are derived once from resolved recipes during acquisition and
saved in `Addon<K>`. Both backends merge these saved values in component slot order,
then dependency order, without resolving shared releases. Virgo caches contain
filesystem and registry effects. Installation still executes ordered environment
steps so commands see the variables declared so far. Explicit owner settings take
precedence; execution-owned variables such as
`WINEPREFIX`, `WINEARCH`, and `PROTONPATH` are applied last. Snapshots capture owner
metadata, addon selections, registry baseline, and private prefix data while
stopped, without WineBridge discovery files.

Standard component removal still uses the shared release’s recipe. Removing that
release preserves runtime variables but makes recipe-based uninstallation unavailable.

Bottle configuration uses version 1. `EnvironmentConfig::backend` selects
`PrefixBackend::Standard` or `PrefixBackend::Virgo` and is serialized under the existing
`environment.storage` key. Layers are derived from selected addons rather than stored
in owner state.
Completed addon caches remain reusable. Snapshots stop the runtime
and capture existing configuration, registry baseline, and private data without
preparing Virgo. Pending selections remain pending after restoration and are
prepared at the next launch. Explicit snapshots create a new commit even when
contents are unchanged; `bottles-next:auto-checkpoint` is a reserved message.

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
