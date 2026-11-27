use super::*;

pub(super) fn collect_suggestions(
    files: &[std::path::PathBuf],
    root: &Path,
    index: Option<Arc<ProjectIndex>>,
) -> Collected {
    let requests: Vec<(String, String)> = files
        .iter()
        .map(|file| (file.to_string_lossy().into_owned(), diff_name(file, root)))
        .collect();
    let mut collected = Collected {
        suggestions: Vec::new(),
        withheld: Vec::new(),
        unreadable: 0,
    };
    for ((display, _), (result, error)) in
        requests
            .iter()
            .zip(suggest_batch(&crate::files(), &requests, index))
    {
        let Some(parsed) = result
            .as_deref()
            .and_then(|j| serde_json::from_str::<Value>(j).ok())
        else {
            // Unreadable, or not valid Python.
            eprintln!("{display}: skipped, {}", error.as_deref().unwrap_or("None"));
            collected.unreadable += 1;
            continue;
        };
        for suggestion in parsed["suggestions"].as_array().into_iter().flatten() {
            collected
                .suggestions
                .push((display.clone(), suggestion.clone()));
        }
        for site in parsed["withheld"].as_array().into_iter().flatten() {
            collected.withheld.push((display.clone(), site.clone()));
        }
    }
    collected
}

// Keeps only the sites called `names`. Exits 2 if a name matches nothing or more than one site,
// and 1 if `editing` and a name is a site that has no suggestion.
pub(super) fn select_named_sites(
    names: &[String],
    suggestions: Vec<Site>,
    withheld: Vec<Site>,
    editing: bool,
) -> Result<(Vec<Site>, Vec<Site>), i32> {
    let mut chosen_suggestions = Vec::new();
    let mut chosen_withheld = Vec::new();
    let mut seen: Vec<&String> = Vec::new();
    for raw in names {
        if seen.contains(&raw) {
            continue;
        }
        seen.push(raw);
        let name = raw.trim().to_lowercase();
        let found_suggestions: Vec<&Site> = suggestions
            .iter()
            .filter(|(_, s)| text(s, "phrase") == name)
            .collect();
        let found_withheld: Vec<&Site> = withheld
            .iter()
            .filter(|(_, w)| text(w, "phrase") == name)
            .collect();
        let total = found_suggestions.len() + found_withheld.len();
        if total == 0 {
            eprintln!("Error: no untyped site is named {}", repr_str(raw));
            return Err(2);
        }
        if total > 1 {
            eprintln!(
                "Error: {} names {total} sites; narrow the path to one of them:",
                repr_str(raw)
            );
            for (display_path, site) in found_suggestions.iter().chain(&found_withheld) {
                eprintln!("  {}  {}", location(display_path, site), text(site, "var"));
            }
            return Err(2);
        }
        if editing {
            if let Some((_, site)) = found_withheld.first() {
                eprintln!(
                    "Error: {} can't be edited: {}",
                    text(site, "phrase"),
                    text(site, "reason")
                );
                return Err(1);
            }
        }
        chosen_suggestions.extend(found_suggestions.into_iter().cloned());
        chosen_withheld.extend(found_withheld.into_iter().cloned());
    }
    Ok((chosen_suggestions, chosen_withheld))
}

// Reads one of y/n/a/q, asking again on anything else; end of input counts as `q`.
fn ask(out: &mut impl Write, input: &mut impl BufRead, prompt: &str) -> io::Result<String> {
    loop {
        write!(out, "{prompt}")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok("q".to_string());
        }
        let answer = line.trim().to_lowercase();
        if matches!(answer.as_str(), "y" | "n" | "a" | "q") {
            return Ok(answer);
        }
        writeln!(out, "Please answer y, n, a or q.")?;
    }
}

fn choose_for(chosen: &mut Chosen, display: &str, origin: (usize, usize)) {
    match chosen.iter_mut().find(|(file, _)| file == display) {
        Some((_, origins)) => origins.push(origin),
        None => chosen.push((display.to_string(), vec![origin])),
    }
}

// Asks about each suggestion in turn and returns the chosen origins by file: `y` applies this one,
// `n` skips it, `a` applies this and every remaining one, `q` quits and applies nothing (`None`).
fn choose_interactively(
    out: &mut impl Write,
    input: &mut impl BufRead,
    suggestions: &[Site],
) -> io::Result<Option<Chosen>> {
    let mut chosen = Chosen::new();
    let mut apply_rest = false;
    for (display_path, suggestion) in suggestions {
        if !apply_rest {
            writeln!(out, "{}", format_suggestion(display_path, suggestion))?;
            let answer = ask(out, input, "Apply this? [y/n/a/q] ")?;
            if answer == "q" {
                writeln!(out, "Quit: nothing was changed.")?;
                return Ok(None);
            }
            if answer == "n" {
                writeln!(out)?;
                continue;
            }
            apply_rest = answer == "a";
            writeln!(out)?;
        }
        choose_for(
            &mut chosen,
            display_path,
            (int(suggestion, "line"), int(suggestion, "col")),
        );
    }
    Ok(Some(chosen))
}

// What to hand to the apply step for --apply, --apply --only and --interactive; `None` if the
// user quit.
pub(super) fn edit_requests(
    out: &mut impl Write,
    input: &mut impl BufRead,
    args: &SuggestArgs,
    suggestions: &[Site],
) -> io::Result<Option<Vec<Request>>> {
    if args.apply && args.only.is_empty() {
        let mut safe_files: Vec<&String> = suggestions
            .iter()
            .filter(|(_, s)| text(s, "tier") == "safe")
            .map(|(display, _)| display)
            .collect();
        safe_files.sort();
        safe_files.dedup();
        return Ok(Some(
            safe_files
                .into_iter()
                .map(|display| (display.clone(), display.clone(), Selection::SafeTier))
                .collect(),
        ));
    }
    let chosen = if args.apply {
        // Naming a site is the choice, so its tier doesn't matter, but its notes are shown first.
        let mut chosen = Chosen::new();
        for (display, suggestion) in suggestions {
            for note in suggestion["notes"].as_array().into_iter().flatten() {
                writeln!(
                    out,
                    "{}: note: {}",
                    text(suggestion, "phrase"),
                    note.as_str().unwrap_or("")
                )?;
            }
            choose_for(
                &mut chosen,
                display,
                (int(suggestion, "line"), int(suggestion, "col")),
            );
        }
        chosen
    } else {
        match choose_interactively(out, input, suggestions)? {
            Some(chosen) => chosen,
            None => return Ok(None),
        }
    };
    Ok(Some(
        chosen
            .into_iter()
            .map(|(display, origins)| (display.clone(), display, Selection::Origins(origins)))
            .collect(),
    ))
}
