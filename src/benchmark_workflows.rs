//! Resolve Cargo benchmark selectors in each job's own static matrix.
use anyhow::Result;
use regex::Regex;
use serde_yaml::{Mapping, Value};

fn matches(row: &Mapping, pattern: &Mapping) -> bool {
    pattern
        .iter()
        .all(|(key, value)| row.get(key) == Some(value))
}

fn matrix_rows(matrix: &Value) -> Vec<Mapping> {
    let Some(matrix) = matrix.as_mapping() else {
        return Vec::new();
    };
    let axes: Vec<_> = matrix
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), Some("include" | "exclude")))
        .collect();
    let mut rows = vec![Mapping::new()];
    for (key, values) in &axes {
        let Some(values) = values.as_sequence() else {
            return Vec::new();
        };
        rows = rows
            .into_iter()
            .flat_map(|row| {
                values.iter().map(move |value| {
                    let mut row = row.clone();
                    row.insert((*key).clone(), value.clone());
                    row
                })
            })
            .collect();
    }
    if let Some(excludes) = matrix.get("exclude").and_then(Value::as_sequence) {
        rows.retain(|row| {
            !excludes
                .iter()
                .filter_map(Value::as_mapping)
                .any(|item| matches(row, item))
        });
    }
    if let Some(includes) = matrix.get("include").and_then(Value::as_sequence) {
        if axes.is_empty() {
            rows.clear();
        }
        for include in includes.iter().filter_map(Value::as_mapping) {
            let mut merged = false;
            for row in &mut rows {
                if !axes.is_empty()
                    && axes.iter().all(|(key, _)| {
                        include
                            .get(*key)
                            .is_none_or(|value| row.get(*key) == Some(value))
                    })
                {
                    row.extend(include.clone());
                    merged = true;
                }
            }
            if !merged {
                rows.push(include.clone());
            }
        }
    }
    rows
}

pub(crate) fn selectors(text: &str) -> Result<Vec<String>> {
    let literal = Regex::new(r#"cargo\s+bench\b[^\n]*?--bench\s+['\"]?([A-Za-z0-9_.*-]+)"#)?;
    let dynamic = Regex::new(
        r#"cargo\s+bench\b[^\n]*?--bench\s+['\"]?\$\{\{\s*matrix\.([A-Za-z0-9_]+)\s*\}\}"#,
    )?;
    let text = text.replace("\\\n", " ");
    let mut selectors: Vec<_> = literal
        .captures_iter(&text)
        .map(|capture| capture[1].to_owned())
        .collect();
    let document: Value = serde_yaml::from_str(&text)?;
    if let Some(jobs) = document.get("jobs").and_then(Value::as_mapping) {
        for job in jobs.values() {
            let rows = matrix_rows(&job["strategy"]["matrix"]);
            if let Some(steps) = job.get("steps").and_then(Value::as_sequence) {
                for run in steps
                    .iter()
                    .filter_map(|step| step.get("run").and_then(Value::as_str))
                {
                    for capture in dynamic.captures_iter(run) {
                        selectors.extend(rows.iter().filter_map(|row| {
                            row.get(&capture[1])
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        }));
                    }
                }
            }
        }
    }
    Ok(selectors)
}
