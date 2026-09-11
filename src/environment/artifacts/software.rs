//! Builds only declared prerequisites over Soda, without owner settings or upper data.

use strum::IntoEnumIterator;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{cache, downloaded_soda, ensure_base};
use crate::{
    AddonError, Addons, Context, EnvVars, EnvironmentError, Progress, Requirement, Slot, Stage,
    addons::{Artifact, InstallInputs, execute, replay_env_vars},
    environment::EnvironmentConfig,
    error::{Error, Result},
};

fn requirements(id: Uuid, config: &EnvironmentConfig) -> Result<&[Requirement]> {
    if let Some(addon) = config.components.values().find(|addon| addon.id() == id) {
        return Ok(addon.requirements());
    }
    Ok(config
        .dependency(id)
        .ok_or(AddonError::NotFound(id))?
        .requirements())
}

/// Runtime requirements supply tools, not layers. Soda always supplies Wine.
fn prerequisites(id: Uuid, config: &EnvironmentConfig) -> Result<Vec<Uuid>> {
    let mut ids = Vec::new();
    for requirement in requirements(id, config)? {
        if let Some(addon) = Slot::iter()
            .filter_map(|slot| config.component(slot))
            .find(|addon| addon.satisfies(requirement))
        {
            if !addon.slot().is_runtime() {
                ids.push(addon.id());
            }
        } else if let Some(addon) = config
            .dependencies
            .iter()
            .find(|addon| addon.satisfies(requirement))
        {
            ids.push(addon.id());
        } else {
            return Err(EnvironmentError::RequiresAddon {
                required_by: Some(id),
                requirements: vec![requirement.clone()],
            }
            .into());
        }
    }
    Ok(ids)
}

fn visit(
    id: Uuid,
    config: &EnvironmentConfig,
    visiting: &mut Vec<Uuid>,
    ordered: &mut Vec<Uuid>,
) -> Result<()> {
    if ordered.contains(&id) {
        return Ok(());
    }
    if visiting.contains(&id) {
        return Err(EnvironmentError::CyclicPrerequisites(id).into());
    }
    visiting.push(id);
    for prerequisite in prerequisites(id, config)? {
        visit(prerequisite, config, visiting, ordered)?;
    }
    visiting.pop();
    ordered.push(id);
    Ok(())
}

fn resources(id: Uuid, addons: &Addons, cx: &Context) -> Result<Vec<Artifact>> {
    if let Some(component) = addons.component(id) {
        return Ok(vec![component.artifact(cx.directories())]);
    }
    let dependency = addons.dependency(id).ok_or(AddonError::NotFound(id))?;
    Ok(dependency.resources(cx.directories()))
}

pub(crate) async fn prepare_addon(
    id: Uuid,
    config: &EnvironmentConfig,
    addons: &Addons,
    cx: &Context,
    progress: &watch::Sender<Option<Progress>>,
    cancellation: &CancellationToken,
) -> Result<()> {
    let _build = cancellation
        .run_until_cancelled(cx.artifact_build().lock())
        .await
        .ok_or(Error::Cancelled)?;
    // UUID alone is the cache identity, independent of owner settings and runner.
    if cache::exists(id, cx).await? {
        return Ok(());
    }
    let base = ensure_base(addons, cx, cancellation).await?;
    let soda = downloaded_soda(base.soda.id(), base.soda.version(), addons)?;
    let runner = soda.load_runner(cx.directories(), None).await?;
    let winebridge = addons
        .latest_component(Slot::WineBridge)
        .ok_or(EnvironmentError::ComponentNotInstalled(Slot::WineBridge))?
        .path(cx.directories());
    let mut order = Vec::new();
    visit(id, config, &mut Vec::new(), &mut order)?;
    for id in order {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if cache::exists(id, cx).await? {
            continue;
        }
        let mut required = Vec::new();
        visit(id, config, &mut Vec::new(), &mut required)?;
        required.pop();
        let mut layers = vec![base.layer.clone()];
        let mut env_vars = EnvVars::default();
        for prerequisite in &required {
            layers.push(cache::layer(*prerequisite, cx).await?);
            replay_env_vars(&mut env_vars, &resources(*prerequisite, addons, cx)?);
        }
        let resources = resources(id, addons, cx)?;
        cache::install(
            layers,
            id,
            &required,
            runner.as_ref(),
            async |prefix| {
                execute(
                    InstallInputs {
                        prefix,
                        runner: runner.as_ref(),
                        winebridge: &winebridge,
                        env_vars: &mut env_vars,
                    },
                    &resources,
                    cancellation,
                    |_| {
                        progress.send_replace(Some(Progress::new(Stage::Configuring)));
                    },
                )
                .await
            },
            cx,
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prerequisite_order_excludes_unrelated_addons_and_rejects_cycles() {
        let ids = [
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        ];
        let dependency = |id: Uuid, requirements: Vec<Requirement>| {
            serde_json::from_value(json!({
                "id": id, "name": id.to_string(), "version": "1.0.0", "requirements": requirements
            }))
            .unwrap()
        };
        let mut config = EnvironmentConfig {
            storage: crate::Storage::Virgo { layers: vec![] },
            components: Default::default(),
            dependencies: vec![
                dependency(
                    ids[0],
                    vec![Requirement::Id(ids[1]), Requirement::Id(ids[2])],
                ),
                dependency(ids[1], vec![Requirement::Id(ids[2])]),
                dependency(ids[2], vec![]),
                dependency(ids[3], vec![]),
            ],
            env_vars: Default::default(),
            wrappers: Default::default(),
        };
        let mut order = Vec::new();
        visit(ids[0], &config, &mut Vec::new(), &mut order).unwrap();
        assert_eq!(order, [ids[2], ids[1], ids[0]]);
        config.dependencies[2] = dependency(ids[2], vec![Requirement::Id(ids[0])]);
        assert!(matches!(
            visit(ids[0], &config, &mut Vec::new(), &mut Vec::new()),
            Err(Error::Environment(EnvironmentError::CyclicPrerequisites(_)))
        ));
    }
}
