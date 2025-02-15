//! This module contains code to resolve python package from PyPi or Conda packages.
//!
//! See [`resolve_pypi`] and [`resolve_conda`] for more information.

use crate::{
    lock_file::{pypi, LockedCondaPackages, LockedPypiPackages, PypiRecord},
    project::manifest::{PyPiRequirement, SystemRequirements},
};
use indexmap::IndexMap;
use indicatif::ProgressBar;
use miette::IntoDiagnostic;
use rattler_conda_types::{GenericVirtualPackage, MatchSpec, Platform, RepoDataRecord};
use rattler_lock::{PackageHashes, PypiPackageData, PypiPackageEnvironmentData};
use rattler_solve::{resolvo, SolverImpl};
use rip::{index::PackageDb, resolve::solve_options::SDistResolution};
use std::{collections::HashMap, path::Path, sync::Arc};

/// This function takes as input a set of dependencies and system requirements and returns a set of
/// locked packages.
#[allow(clippy::too_many_arguments)]
pub async fn resolve_pypi(
    package_db: Arc<PackageDb>,
    dependencies: IndexMap<rip::types::PackageName, Vec<PyPiRequirement>>,
    system_requirements: SystemRequirements,
    locked_conda_records: &[RepoDataRecord],
    _locked_pypi_records: &[PypiRecord],
    platform: Platform,
    pb: &ProgressBar,
    python_location: Option<&Path>,
    sdist_resolution: SDistResolution,
    env_variables: HashMap<String, String>,
) -> miette::Result<LockedPypiPackages> {
    // Solve python packages
    pb.set_message("resolving pypi dependencies");
    let python_artifacts = pypi::resolve_dependencies(
        package_db.clone(),
        dependencies,
        system_requirements,
        platform,
        locked_conda_records,
        python_location,
        sdist_resolution,
        env_variables,
    )
    .await?;

    // Clear message
    pb.set_message("");

    // Add pip packages
    let mut locked_packages = LockedPypiPackages::with_capacity(python_artifacts.len());
    for python_artifact in python_artifacts {
        let (artifact, metadata) = package_db
            // No need for a WheelBuilder here since any builds should have been done during the
            // [`python::resolve_dependencies`] call.
            .get_metadata(&python_artifact.artifacts, None)
            .await
            .expect("failed to get metadata for a package for which we have already fetched metadata during solving.")
            .expect("no metadata for a package for which we have already fetched metadata during solving.");

        let pkg_data = PypiPackageData {
            name: python_artifact.name.to_string(),
            version: python_artifact.version,
            requires_dist: metadata.requires_dist,
            requires_python: metadata.requires_python,
            url: artifact.url.clone(),
            hash: artifact
                .hashes
                .as_ref()
                .and_then(|hash| PackageHashes::from_hashes(None, hash.sha256)),
        };

        let pkg_env = PypiPackageEnvironmentData {
            extras: python_artifact
                .extras
                .into_iter()
                .map(|e| e.as_str().to_string())
                .collect(),
        };

        locked_packages.push((pkg_data, pkg_env));
    }

    Ok(locked_packages)
}

/// Solves the conda package environment for the given input. This function is async because it
/// spawns a background task for the solver. Since solving is a CPU intensive task we do not want to
/// block the main task.
pub async fn resolve_conda(
    specs: Vec<MatchSpec>,
    virtual_packages: Vec<GenericVirtualPackage>,
    locked_packages: Vec<RepoDataRecord>,
    available_packages: Vec<Vec<RepoDataRecord>>,
) -> miette::Result<LockedCondaPackages> {
    tokio::task::spawn_blocking(move || {
        // Construct a solver task that we can start solving.
        let task = rattler_solve::SolverTask {
            specs,
            available_packages: &available_packages,
            locked_packages,
            pinned_packages: vec![],
            virtual_packages,
            timeout: None,
        };

        // Solve the task
        resolvo::Solver.solve(task).into_diagnostic()
    })
    .await
    .unwrap_or_else(|e| match e.try_into_panic() {
        Ok(e) => std::panic::resume_unwind(e),
        Err(_err) => Err(miette::miette!("cancelled")),
    })
}
