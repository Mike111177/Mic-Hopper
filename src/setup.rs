//! Interactive first-run setup: pick devices from a numbered list and write
//! a fully commented config file.

use anyhow::{bail, Context, Result};
use std::io::{self, BufRead, Write};
use std::path::Path;

use crate::audio::{self, DeviceInfo};
use crate::config::DEFAULT_CONFIG_TOML;

pub fn run(host: &cpal::Host, path: &Path) -> Result<()> {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();

    if path.exists() {
        println!("{} already exists.", path.display());
        if !confirm(&mut lines, "Overwrite it? [y/N] ", false)? {
            println!("Keeping the existing config.");
            return Ok(());
        }
    }

    let (ins, outs) = audio::list_devices(host)?;
    if ins.len() < 2 {
        bail!("need at least two input devices, found {}", ins.len());
    }
    if outs.is_empty() {
        bail!("no output devices found");
    }

    println!("\nInput devices:");
    print_devices(&ins);
    let desktop = pick(&mut lines, "Desktop microphone", ins.len(), None)?;
    let headset = pick(&mut lines, "Headset microphone", ins.len(), None)?;
    if desktop == headset {
        bail!("desktop and headset must be different devices");
    }

    println!("\nOutput devices (choose the virtual cable's playback side, e.g. \"CABLE Input\"):");
    print_devices(&outs);
    let cable_default = outs.iter().position(|d| d.name.contains("CABLE Input"));
    let output = pick(&mut lines, "Virtual microphone output", outs.len(), cable_default)?;
    if outs[output].name.contains("CABLE Output") {
        println!("Warning: \"CABLE Output\" is the recording side; you almost certainly want \"CABLE Input\".");
    }

    let desktop_name = &ins[desktop].name;
    let headset_name = &ins[headset].name;
    let output_name = &outs[output].name;

    let rates = [ins[desktop].sample_rate, ins[headset].sample_rate, outs[output].sample_rate];
    if rates.iter().any(|&r| r != rates[0]) {
        println!(
            "\nNote: sample rates differ ({} / {} / {} Hz). This works, but matching them\n\
             (Sound Control Panel > Properties > Advanced) gives the best quality.",
            rates[0], rates[1], rates[2]
        );
    }

    let text = DEFAULT_CONFIG_TOML
        .replacen("desktop = \"CHANGE ME\"", &format!("desktop = {}", toml_str(desktop_name)), 1)
        .replacen("headset = \"CHANGE ME\"", &format!("headset = {}", toml_str(headset_name)), 1)
        .replacen("output = \"CABLE Input\"", &format!("output = {}", toml_str(output_name)), 1);

    // Make sure what we wrote is loadable before claiming success.
    let _: crate::config::Config =
        toml::from_str(&text).context("generated config did not parse (please report this)")?;
    std::fs::write(path, &text).with_context(|| format!("writing {}", path.display()))?;

    println!("\nWrote {}", path.display());
    println!("  desktop = {desktop_name}");
    println!("  headset = {headset_name}");
    println!("  output  = {output_name}");
    println!("\nNext: run `mike-hopper --monitor` to calibrate desktop_offset_db, then `mike-hopper`.");
    Ok(())
}

fn print_devices(devs: &[DeviceInfo]) {
    for (i, d) in devs.iter().enumerate() {
        println!("  [{}] {:<55} {} Hz, {} ch", i + 1, d.name, d.sample_rate, d.channels);
    }
}

fn pick(
    lines: &mut impl Iterator<Item = io::Result<String>>,
    what: &str,
    count: usize,
    default: Option<usize>,
) -> Result<usize> {
    loop {
        match default {
            Some(d) => print!("{what} [1-{count}, default {}]: ", d + 1),
            None => print!("{what} [1-{count}]: "),
        }
        io::stdout().flush()?;
        let line = match lines.next() {
            Some(l) => l?,
            None => bail!("stdin closed"),
        };
        let line = line.trim();
        if line.is_empty() {
            if let Some(d) = default {
                return Ok(d);
            }
            continue;
        }
        match line.parse::<usize>() {
            Ok(n) if (1..=count).contains(&n) => return Ok(n - 1),
            _ => println!("Please enter a number between 1 and {count}."),
        }
    }
}

fn confirm(
    lines: &mut impl Iterator<Item = io::Result<String>>,
    prompt: &str,
    default: bool,
) -> Result<bool> {
    print!("{prompt}");
    io::stdout().flush()?;
    let line = match lines.next() {
        Some(l) => l?,
        None => bail!("stdin closed"),
    };
    Ok(match line.trim().to_lowercase().as_str() {
        "" => default,
        "y" | "yes" => true,
        _ => false,
    })
}

/// Quote a string as a TOML basic string.
fn toml_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_str_escapes() {
        assert_eq!(toml_str(r#"Mic "A" \ B"#), r#""Mic \"A\" \\ B""#);
        let parsed: toml::Value =
            toml::from_str(&format!("x = {}", toml_str(r#"Mic "A" \ B"#))).unwrap();
        assert_eq!(parsed["x"].as_str().unwrap(), r#"Mic "A" \ B"#);
    }

    #[test]
    fn template_substitution_targets_exist() {
        assert!(DEFAULT_CONFIG_TOML.contains("desktop = \"CHANGE ME\""));
        assert!(DEFAULT_CONFIG_TOML.contains("headset = \"CHANGE ME\""));
        assert!(DEFAULT_CONFIG_TOML.contains("output = \"CABLE Input\""));
    }
}
