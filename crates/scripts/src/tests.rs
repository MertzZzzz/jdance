// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use jdance_domain::Note;

fn n(id: u32, start: u32, length: u32, key: u8, vel: u8) -> Note {
    Note {
        id,
        start,
        length,
        key,
        velocity: nt::stored_velocity(i32::from(vel)),
    }
}

fn input<'a>(notes: &'a [Note], selected: &'a [usize]) -> Input<'a> {
    Input {
        notes,
        selected,
        pattern_len: 3840,
        grid: 240,
        range: None,
        signature: jdance_domain::TimeSignature::COMMON,
    }
}

fn builtin(name: &str) -> Script {
    builtins()
        .into_iter()
        .find(|s| s.info.name == name)
        .unwrap()
}

fn run_with(
    s: &Script,
    values: Vec<(&str, ParamValue)>,
    seed: u64,
    inp: &Input,
) -> Result<ScriptOutput, String> {
    let run = ScriptRun {
        id: s.info.id.clone(),
        hash: s.info.hash,
        values: values.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        seed,
    };
    s.run(&run, inp, Arc::new(AtomicBool::new(false)))
}

fn notes_of(o: ScriptOutput) -> nt::Outcome {
    match o {
        ScriptOutput::Notes(o) => o,
        ScriptOutput::Selection(_) => panic!("expected notes"),
    }
}

#[test]
fn builtins_load_with_their_parameters() {
    let all = builtins();
    let names: Vec<&str> = all.iter().map(|s| s.info.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Arpeggiator",
            "Euclidean",
            "Humanize",
            "Note repeater sprinkler",
            "Select by velocity and more"
        ]
    );
    let e = builtin("Euclidean");
    assert_eq!(e.info.params[0].key, "steps");
    assert_eq!(e.info.params[0].default, ParamValue::Int(16));
    assert!(builtin("Select by velocity and more").info.selects);
}

#[test]
fn bjorklund_golden() {
    let s = |steps, pulses| -> String {
        bjorklund(steps, pulses)
            .into_iter()
            .map(|h| if h { 'x' } else { '.' })
            .collect()
    };
    assert_eq!(s(8, 3), "x..x..x.");
    assert_eq!(s(8, 5), "x.xx.xx.");
    assert_eq!(s(16, 4), "x...x...x...x...");
    assert_eq!(s(13, 5), "x..x.x..x.x..");
    assert_eq!(s(5, 0), ".....");
    assert_eq!(s(4, 4), "xxxx");
    assert_eq!(s(3, 9), "xxx", "pulses clamp to steps");
    assert!(s(0, 0).is_empty());
}

#[test]
fn euclidean_generates_over_the_range() {
    let inp = Input {
        range: full(3840),
        ..input(&[], &[])
    };
    let e = builtin("Euclidean");
    let o = notes_of(
        run_with(
            &e,
            vec![
                ("steps", ParamValue::Int(8)),
                ("pulses", ParamValue::Int(3)),
                ("step", ParamValue::Int(480)),
            ],
            1,
            &inp,
        )
        .unwrap(),
    );
    let starts: Vec<u32> = o.notes.iter().map(|n| n.start).collect();
    assert_eq!(starts, [0, 1440, 2880]);
    assert_eq!(o.notes[0].length, 240, "gate 50 %");
    let none = notes_of(run_with(&e, vec![("pulses", ParamValue::Int(0))], 1, &inp).unwrap());
    assert!(none.notes.is_empty());
    let rotated = notes_of(
        run_with(
            &e,
            vec![
                ("steps", ParamValue::Int(8)),
                ("pulses", ParamValue::Int(3)),
                ("step", ParamValue::Int(480)),
                ("rotation", ParamValue::Int(1)),
            ],
            1,
            &inp,
        )
        .unwrap(),
    );
    assert_eq!(rotated.notes[0].start, 480);
}

#[test]
fn humanize_is_seeded_bounded_and_keeps_ids() {
    let notes = [
        n(1, 0, 240, 60, 100),
        n(2, 0, 240, 64, 100),
        n(3, 960, 240, 67, 100),
    ];
    let h = builtin("Humanize");
    let a = notes_of(run_with(&h, vec![], 7, &input(&notes, &[])).unwrap());
    let b = notes_of(run_with(&h, vec![], 7, &input(&notes, &[])).unwrap());
    assert_eq!(a, b, "same seed, same result");
    let c = notes_of(run_with(&h, vec![], 8, &input(&notes, &[])).unwrap());
    assert_ne!(a, c);
    let mut ids: Vec<u32> = a.notes.iter().map(|n| n.id).collect();
    ids.sort_unstable();
    assert_eq!(ids, [1, 2, 3]);
    // Chords move together.
    let s1 = a.notes.iter().find(|n| n.id == 1).unwrap().start;
    let s2 = a.notes.iter().find(|n| n.id == 2).unwrap().start;
    assert_eq!(s1, s2);
    assert!(a.notes.iter().all(|n| n.start < 3840 && n.length >= 1));
}

#[test]
fn sprinkler_at_probability_one_matches_chop() {
    let notes = [n(5, 0, 1000, 60, 90), n(6, 1200, 300, 62, 90)];
    let sp = builtin("Note repeater sprinkler");
    let o = notes_of(
        run_with(
            &sp,
            vec![
                ("probability", ParamValue::Float(1.0)),
                ("step", ParamValue::Int(240)),
                ("gate", ParamValue::Int(50)),
            ],
            3,
            &input(&notes, &[]),
        )
        .unwrap(),
    );
    let chop = nt::chop(
        &input(&notes, &[]),
        &nt::Chop {
            mode: nt::ChopMode::Step(240),
            gate_percent: 50,
            ..nt::Chop::default()
        },
    )
    .unwrap();
    assert_eq!(o.notes, chop.notes);
    let gone = notes_of(
        run_with(
            &sp,
            vec![("probability", ParamValue::Float(0.0))],
            3,
            &input(&notes, &[]),
        )
        .unwrap(),
    );
    assert!(gone.notes.is_empty());
}

#[test]
fn select_by_velocity_changes_only_the_selection() {
    let notes = [
        n(1, 0, 240, 60, 30),
        n(2, 240, 240, 62, 100),
        n(3, 480, 240, 64, 120),
    ];
    let s = builtin("Select by velocity and more");
    let pick = |values, selected: &[usize]| match run_with(&s, values, 1, &input(&notes, selected))
        .unwrap()
    {
        ScriptOutput::Selection(v) => v,
        ScriptOutput::Notes(_) => panic!("expected a selection"),
    };
    assert_eq!(
        pick(vec![("velocity_min", ParamValue::Int(90))], &[]),
        [1, 2]
    );
    assert_eq!(
        pick(
            vec![
                ("velocity_min", ParamValue::Int(90)),
                ("mode", ParamValue::Choice(1)),
            ],
            &[0]
        ),
        [0, 1, 2],
        "add"
    );
    assert_eq!(
        pick(
            vec![
                ("velocity_min", ParamValue::Int(110)),
                ("mode", ParamValue::Choice(2)),
            ],
            &[1, 2]
        ),
        [1],
        "subtract"
    );
}

#[test]
fn arpeggiator_script_matches_the_built_in_tool() {
    let notes = [
        n(1, 0, 960, 60, 100),
        n(2, 0, 960, 64, 90),
        n(3, 0, 960, 67, 80),
    ];
    let inp = input(&notes, &[]);
    let a = notes_of(
        run_with(
            &builtin("Arpeggiator"),
            vec![("direction", ParamValue::Choice(2))],
            5,
            &inp,
        )
        .unwrap(),
    );
    let b = nt::arpeggiate(
        &inp,
        &nt::Arpeggiate {
            direction: nt::ArpDirection::UpDown,
            step: 240,
            gate_percent: 90,
            octaves: 1,
            seed: 5,
        },
    )
    .unwrap();
    assert_eq!(a.notes, b.notes);
}

#[test]
fn bad_scripts_fail_without_effect() {
    let notes = [n(1, 0, 240, 60, 100)];
    let inp = input(&notes, &[]);
    let parse = |src: &str| Script::parse("t".into(), "t", src, None);
    assert!(parse("fn params( {").is_err(), "syntax error");
    assert!(parse("fn run(ctx) { 1 }").is_err(), "no params()");
    let looping = parse("fn params() { [] } fn run(ctx) { loop { } }").unwrap();
    let err = run_with(&looping, vec![], 1, &inp).unwrap_err();
    assert!(
        err.contains("operations") || err.contains("stopped"),
        "{err}"
    );
    let bad_key = parse(
        "fn params() { [] } fn run(ctx) { #{ notes: [#{ start: 0, length: 1, key: 300, velocity: 1 }] } }",
    )
    .unwrap();
    assert!(
        run_with(&bad_key, vec![], 1, &inp)
            .unwrap_err()
            .contains("key")
    );
    let bad_id = parse("fn params() { [] } fn run(ctx) { #{ select: [99] } }").unwrap();
    assert!(run_with(&bad_id, vec![], 1, &inp).is_err());
    let no_eval = parse(r#"fn params() { [] } fn run(ctx) { eval("1") }"#);
    assert!(no_eval.is_err(), "eval is disabled");
    let import = parse(r#"import "x" as y; fn params() { [] } fn run(ctx) { 1 }"#);
    assert!(import.is_err(), "no modules");
    // Cancel stops a run.
    let cancel = Arc::new(AtomicBool::new(true));
    let run = ScriptRun {
        id: "t".into(),
        hash: 0,
        values: Vec::new(),
        seed: 1,
    };
    let err = looping.run(&run, &inp, cancel).unwrap_err();
    assert!(err.contains("stopped"), "{err}");
}

#[test]
fn user_folder_loads_scripts_and_reports_broken_ones() {
    let dir = std::env::temp_dir().join(format!("jdance-scripts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("a.rhai"),
        "// name: Mine\nfn params() { [] }\nfn run(ctx) { #{ notes: [] } }",
    )
    .unwrap();
    std::fs::write(dir.join("b.rhai"), "fn params( {").unwrap();
    std::fs::write(dir.join("c.txt"), "not a script").unwrap();
    let (ok, errors) = load_folder(&dir);
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0].info.name, "Mine");
    assert_eq!(errors.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
