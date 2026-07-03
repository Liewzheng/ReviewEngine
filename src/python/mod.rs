//! Python bindings for the review-engine library. Allows calling review engine functionality from Python.
//!
//! This module is part of the review-engine CodeReview Board platform.
//!
//!
//! @module review-engine
use crate::models::*;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

#[pyfunction]
fn review(
    mr_url: String,
    gitlab_token: String,
    llm_configs_json: String,
    config_toml: Option<String>,
) -> PyResult<String> {
    let llm_configs: Vec<LLMConfig> = serde_json::from_str(&llm_configs_json)
        .map_err(|e| PyRuntimeError::new_err(format!("Invalid LLM configs: {}", e)))?;

    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to start runtime: {}", e)))?;

    let config_source = config_toml.map(|s| ConfigSource::Inline(s));

    let result =
        rt.block_on(async { crate::run_review(&mr_url, &gitlab_token, llm_configs, config_source, None).await });

    match result {
        Ok(output) => {
            serde_json::to_string(&output).map_err(|e| PyRuntimeError::new_err(format!("Serialization error: {}", e)))
        }
        Err(e) => Err(PyRuntimeError::new_err(format!("Review failed: {}", e))),
    }
}

#[pymodule]
fn review_engine(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(review, m)?)?;
    Ok(())
}
