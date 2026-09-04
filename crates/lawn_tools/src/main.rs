//! Headless generator inspection, deterministic fuzzing, and timing tools.

use std::{env, process::ExitCode, time::Instant};

use anyhow::{Context, Result, bail};
use lawn_core::{
    GameConfig, GeneratorConfig, PlanetGenerator, WorldSeed, planet::CURRENT_GENERATOR_VERSION,
};
use serde_json::json;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("validate") => {
            let (count, start, quick) = parse_validation_args(args)?;
            validate_batch(count, start, quick)
        }
        Some("inspect") => {
            let seed = args
                .next()
                .context("inspect requires a hexadecimal, decimal, or phrase seed")?
                .parse::<WorldSeed>()?;
            if let Some(extra) = args.next() {
                bail!("unexpected argument {extra:?}; quote a seed phrase as one argument");
            }
            inspect(seed)
        }
        Some("help" | "--help" | "-h") | None => {
            print_help();
            Ok(())
        }
        Some(command) => bail!("unknown command {command:?}; run `lawn_tools help`"),
    }
}

fn parse_validation_args(args: impl Iterator<Item = String>) -> Result<(usize, u64, bool)> {
    let mut positional = Vec::with_capacity(2);
    let mut quick = false;
    for arg in args {
        if arg == "--quick" {
            if quick {
                bail!("--quick may only be specified once");
            }
            quick = true;
        } else if arg.starts_with('-') {
            bail!("unknown validate option {arg:?}");
        } else if positional.len() < 2 {
            positional.push(arg);
        } else {
            bail!("unexpected validate argument {arg:?}");
        }
    }
    let mut positional = positional.into_iter();
    Ok((
        parse_count(positional.next(), 1_000)?,
        parse_u64(positional.next(), 0)?,
        quick,
    ))
}

fn parse_count(value: Option<String>, default: usize) -> Result<usize> {
    value.map_or(Ok(default), |value| {
        let count = value
            .parse::<usize>()
            .context("seed count must be a positive integer")?;
        if count == 0 {
            bail!("seed count must be greater than zero");
        }
        Ok(count)
    })
}

fn parse_u64(value: Option<String>, default: u64) -> Result<u64> {
    value.map_or(Ok(default), |value| {
        value
            .parse::<u64>()
            .context("start value must be an unsigned integer")
    })
}

fn validate_batch(count: usize, start: u64, quick: bool) -> Result<()> {
    let config = if quick {
        GeneratorConfig::test_quality()
    } else {
        GameConfig::shipping()
            .context("shipping gameplay config is invalid")?
            .generator
    };
    let generator = PlanetGenerator::new(CURRENT_GENERATOR_VERSION, config);
    let started = Instant::now();
    let mut timings_ms = Vec::new();
    timings_ms
        .try_reserve_exact(count)
        .context("requested seed count is too large for the timing report")?;
    let mut attempts = [0_u64; 8];
    let mut ratios = [f64::MAX, f64::MIN];
    let mut reachable_min = 1.0_f64;
    let mut failures = Vec::new();

    for index in 0..count {
        let seed = WorldSeed(splitmix64(start.wrapping_add(index as u64)));
        let seed_started = Instant::now();
        match generator.generate_with_roots(seed, false) {
            Ok(planet) => {
                timings_ms.push(seed_started.elapsed().as_secs_f64() * 1_000.0);
                attempts[planet.generation_attempt.min(7) as usize] += 1;
                ratios[0] = ratios[0].min(planet.validation.mowable_ratio);
                ratios[1] = ratios[1].max(planet.validation.mowable_ratio);
                reachable_min = reachable_min.min(planet.validation.reachable_ratio);
            }
            Err(error) => failures.push(json!({
                "seed": seed.to_string(),
                "error": error.to_string(),
            })),
        }
    }
    timings_ms.sort_by(f64::total_cmp);
    let elapsed = started.elapsed().as_secs_f64();
    let report = json!({
        "generator_version": CURRENT_GENERATOR_VERSION.0,
        "quality": if quick { "quick" } else { "shipping" },
        "requested_seeds": count,
        "start_index": start,
        "valid_seeds": count - failures.len(),
        "failures": failures,
        "mowable_ratio": {
            "minimum": (!timings_ms.is_empty()).then_some(ratios[0]),
            "maximum": (!timings_ms.is_empty()).then_some(ratios[1]),
        },
        "minimum_reachable_ratio": (!timings_ms.is_empty()).then_some(reachable_min),
        "generation_attempt_histogram": attempts,
        "timing_ms": {
            "median": percentile(&timings_ms, 0.50),
            "p95": percentile(&timings_ms, 0.95),
            "maximum": timings_ms.last().copied().unwrap_or_default(),
            "total_seconds": elapsed,
        }
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report["failures"]
        .as_array()
        .is_some_and(|items| !items.is_empty())
    {
        bail!("one or more generated planets failed validation");
    }
    Ok(())
}

fn inspect(seed: WorldSeed) -> Result<()> {
    let started = Instant::now();
    let planet = PlanetGenerator::default().generate(seed)?;
    let report = json!({
        "generator_version": planet.generator_version.0,
        "world_seed": planet.world_seed.to_string(),
        "attempt": planet.generation_attempt,
        "deterministic_hash": format!("{:016X}", planet.deterministic_hash),
        "mountain_count": planet.mountains.len(),
        "terrain_cells": planet.terrain.len(),
        "grass_roots": planet.grass_roots.len(),
        "grass_patches": planet.grass_patches.len(),
        "spawn": {
            "position": planet.spawn.position.to_array(),
            "clearance": planet.spawn.clearance,
        },
        "validation": planet.validation,
        "generation_ms": started.elapsed().as_secs_f64() * 1_000.0,
    });
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index]
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn print_help() {
    println!(
        "Lawn Orbit headless tools\n\n\
         Usage:\n\
           lawn_tools validate [COUNT] [START] [--quick]\n\
           lawn_tools inspect <SEED>\n\n\
         `validate` defaults to 1,000 shipping-resolution seeds without cosmetic roots.\n\
         `--quick` uses the same generator stages at reduced test resolution."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_flag_is_independent_of_optional_positionals() {
        for (args, expected) in [
            (vec![], (1_000, 0, false)),
            (vec!["--quick"], (1_000, 0, true)),
            (vec!["100", "--quick"], (100, 0, true)),
            (vec!["100", "500", "--quick"], (100, 500, true)),
            (vec!["--quick", "100", "500"], (100, 500, true)),
        ] {
            assert_eq!(
                parse_validation_args(args.into_iter().map(str::to_owned)).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn invalid_validation_arguments_are_rejected() {
        for args in [
            vec!["0"],
            vec!["100", "nope"],
            vec!["--quik"],
            vec!["1", "2", "3"],
            vec!["--quick", "--quick"],
        ] {
            assert!(parse_validation_args(args.into_iter().map(str::to_owned)).is_err());
        }
    }

    #[test]
    fn percentile_handles_empty_and_bounds() {
        assert_eq!(percentile(&[], 0.95), 0.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0], 0.0), 1.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0], 1.0), 3.0);
    }

    #[test]
    fn arbitrary_seed_stream_is_stable() {
        assert_eq!(splitmix64(0), 0xE220_A839_7B1D_CDAF);
    }
}
