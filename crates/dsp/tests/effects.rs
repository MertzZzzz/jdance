// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in effect tests (spec "DSP tests").

use jdance_domain::EffectKind;
use jdance_dsp::eq::{BandKind, Coeffs};
use jdance_dsp::{
    Compressor, ParamEvent, ParametricEq, ProcessContext, Processor, Reverb, SafetyLimiter,
    TempoDelay, process_with_events,
};

const CTX: ProcessContext<'static> = ProcessContext {
    sidechain: None,
    tempo_bpm: 120.0,
};

/// Builds a prepared processor with domain defaults plus `overrides`.
fn make(kind: EffectKind, sr: u32, overrides: &[(&str, f32)]) -> Box<dyn Processor> {
    let mut p: Box<dyn Processor> = match kind {
        EffectKind::ParametricEq => Box::new(ParametricEq::new()),
        EffectKind::TempoDelay => Box::new(TempoDelay::new()),
        EffectKind::Reverb => Box::new(Reverb::new()),
        EffectKind::Compressor => Box::new(Compressor::new()),
        EffectKind::Plugin => unreachable!("plugins are not DSP built-ins"),
    };
    for (i, spec) in kind.params().iter().enumerate() {
        let v = overrides
            .iter()
            .find(|(id, _)| *id == spec.id)
            .map_or(spec.default, |(_, v)| *v);
        p.set_param(i, v);
    }
    p.prepare(sr, 1024);
    p.settle();
    p
}

fn noise(n: usize, seed: u32, amp: f32) -> Vec<f32> {
    let mut x = seed.max(1);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 / u32::MAX as f32 * 2.0 - 1.0) * amp
        })
        .collect()
}

fn sine(n: usize, freq: f64, sr: u32) -> Vec<f32> {
    (0..n)
        .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / f64::from(sr)).sin() as f32)
        .collect()
}

fn impulse(n: usize) -> Vec<f32> {
    let mut v = vec![0.0; n];
    v[0] = 1.0;
    v
}

fn run(
    p: &mut dyn Processor,
    input_l: &[f32],
    input_r: &[f32],
    block: usize,
) -> (Vec<f32>, Vec<f32>) {
    let (mut l, mut r) = (input_l.to_vec(), input_r.to_vec());
    for (cl, cr) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
        p.process_block(cl, cr, &CTX);
    }
    (l, r)
}

fn rms(x: &[f32]) -> f64 {
    (x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
}

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

const ALL: [EffectKind; 4] = EffectKind::ALL;

#[test]
fn param_order_matches_domain_schema() {
    let check = |kind: EffectKind, ids: &[&str]| {
        let schema: Vec<&str> = kind.params().iter().map(|p| p.id).collect();
        assert_eq!(schema, ids, "{kind:?}");
    };
    check(EffectKind::ParametricEq, jdance_dsp::eq::PARAM_IDS);
    check(EffectKind::TempoDelay, jdance_dsp::tempo_delay::PARAM_IDS);
    check(EffectKind::Reverb, jdance_dsp::reverb::PARAM_IDS);
    check(EffectKind::Compressor, jdance_dsp::compressor::PARAM_IDS);
}

// ---------- EQ ----------

#[test]
fn eq_is_unity_at_zero_gain() {
    for sr in [44_100, 48_000, 96_000] {
        let mut eq = make(EffectKind::ParametricEq, sr, &[]);
        let x = noise(10_000, 7, 0.8);
        let (l, r) = run(eq.as_mut(), &x, &x, 333);
        assert_eq!(l, x);
        assert_eq!(r, x);
    }
}

#[test]
fn eq_response_near_set_frequencies() {
    let sr = 48_000;
    // Bell +12 dB at 1 kHz.
    let mut eq = make(
        EffectKind::ParametricEq,
        sr,
        &[("mid1_freq", 1_000.0), ("mid1_gain", 12.0), ("mid1_q", 1.0)],
    );
    let measure = |eq: &mut dyn Processor, f: f64| {
        let x = sine(48_000, f, sr);
        let (l, _) = run(eq, &x, &x, 256);
        db(rms(&l[24_000..]) / rms(&x[24_000..]))
    };
    assert!((measure(eq.as_mut(), 1_000.0) - 12.0).abs() < 0.2);
    assert!(measure(eq.as_mut(), 60.0).abs() < 0.5);
    // Low shelf +6 dB at 200 Hz: full boost well below, none well above.
    let mut eq = make(
        EffectKind::ParametricEq,
        sr,
        &[("low_freq", 200.0), ("low_gain", 6.0)],
    );
    assert!((measure(eq.as_mut(), 30.0) - 6.0).abs() < 0.5);
    assert!(measure(eq.as_mut(), 8_000.0).abs() < 0.2);
    // High shelf −12 dB at 4 kHz.
    let mut eq = make(
        EffectKind::ParametricEq,
        sr,
        &[("high_freq", 4_000.0), ("high_gain", -12.0)],
    );
    assert!((measure(eq.as_mut(), 16_000.0) + 12.0).abs() < 0.6);
    // Analytic response used by the UI graph agrees with measurement.
    let c = Coeffs::design(BandKind::Bell, 1_000.0, 12.0, 1.0, sr);
    assert!((c.response_db(1_000.0, sr) - 12.0).abs() < 1e-6);
}

#[test]
fn eq_band_off_is_unity_and_frequency_is_clamped() {
    let sr = 48_000;
    let mut eq = make(
        EffectKind::ParametricEq,
        sr,
        &[("mid1_gain", 18.0), ("mid1_on", 0.0)],
    );
    let x = noise(4_000, 3, 0.5);
    assert_eq!(run(eq.as_mut(), &x, &x, 64).0, x);
    // 20 kHz at 32 kHz sample rate clamps to 0.45 · 32 kHz = 14.4 kHz.
    assert_eq!(jdance_dsp::eq::max_freq(32_000), 14_400.0);
    let a = Coeffs::design(BandKind::Bell, 20_000.0, 6.0, 1.0, 32_000);
    let b = Coeffs::design(BandKind::Bell, 14_400.0, 6.0, 1.0, 32_000);
    assert_eq!(a, b);
}

#[test]
fn eq_is_stable_at_extreme_parameters() {
    for sr in [44_100, 96_000] {
        for (f, g, q) in [
            (20.0, 24.0, 18.0),
            (20_000.0, -24.0, 0.1),
            (20.0, -24.0, 0.1),
            (20_000.0, 24.0, 18.0),
        ] {
            let mut eq = make(
                EffectKind::ParametricEq,
                sr,
                &[
                    ("low_freq", f),
                    ("low_gain", g),
                    ("mid1_freq", f),
                    ("mid1_gain", g),
                    ("mid1_q", q),
                    ("mid2_freq", f),
                    ("mid2_gain", g),
                    ("mid2_q", q),
                    ("high_freq", f),
                    ("high_gain", g),
                ],
            );
            let mut x = noise(sr as usize, 11, 1.0);
            x[100] = 1.0;
            let (l, _) = run(eq.as_mut(), &x, &x, 512);
            assert!(
                l.iter().all(|v| v.is_finite()),
                "NaN/Inf at f={f} g={g} q={q}"
            );
            assert!(
                l.iter().all(|v| v.abs() < 1e4),
                "unbounded at f={f} g={g} q={q}"
            );
        }
    }
}

#[test]
fn eq_parameter_change_is_smoothed() {
    let sr = 48_000;
    let mut eq = make(EffectKind::ParametricEq, sr, &[("mid1_freq", 1_000.0)]);
    let x = sine(20_000, 1_000.0, sr);
    let (mut l, mut r) = (x.clone(), x.clone());
    eq.process_block(&mut l[..10_000], &mut r[..10_000], &CTX);
    eq.set_param(5, 24.0); // mid1_gain jump
    eq.process_block(&mut l[10_000..], &mut r[10_000..], &CTX);
    // Over the 20 ms ramp the per-sample change stays far below a jump.
    let max_step = l
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    assert!(max_step < 2.5, "step {max_step}");
}

// ---------- Delay ----------

fn first_nonzero(x: &[f32]) -> Option<usize> {
    x.iter().position(|v| v.abs() > 1e-6)
}

#[test]
fn delay_first_echo_position_and_feedback_decay() {
    let sr = 48_000;
    let mut d = make(
        EffectKind::TempoDelay,
        sr,
        &[
            ("sync", 0.0),
            ("time_ms", 100.0),
            ("feedback", 0.5),
            ("wet", 1.0),
            ("lowpass", 20_000.0),
        ],
    );
    let x = impulse(30_000);
    let (l, _) = run(d.as_mut(), &x, &x, 300);
    assert_eq!(first_nonzero(&l), Some(4_800));
    // Echo k carries feedback^(k−1); the low-pass preserves the sum.
    let echo = |k: usize| {
        l[k * 4_800..k * 4_800 + 1_000]
            .iter()
            .map(|v| f64::from(*v))
            .sum::<f64>()
    };
    assert!((echo(1) - 1.0).abs() < 1e-4);
    assert!((echo(2) - 0.5).abs() < 1e-3);
    assert!((echo(3) - 0.25).abs() < 1e-3);
}

#[test]
fn delay_sync_follows_tempo() {
    let sr = 48_000;
    for (bpm, expected) in [(120.0, 24_000), (90.0, 32_000), (60.0, 48_000)] {
        // 1/4 note.
        let mut d = make(
            EffectKind::TempoDelay,
            sr,
            &[
                ("sync", 1.0),
                ("division", 2.0),
                ("feedback", 0.0),
                ("wet", 1.0),
            ],
        );
        let ctx = ProcessContext {
            sidechain: None,
            tempo_bpm: bpm,
        };
        // The first block brings the tempo: let any crossfade from the
        // initial hint settle on silence first.
        let mut warm = vec![0.0; 10_000];
        let mut warm_r = warm.clone();
        d.process_block(&mut warm, &mut warm_r, &ctx);
        let x = impulse(60_000);
        let (mut l, mut r) = (x.clone(), x.clone());
        d.process_block(&mut l, &mut r, &ctx);
        assert_eq!(first_nonzero(&l), Some(expected), "{bpm} BPM");
    }
    // Synced time is not limited to 2 s: 1/2 note at 30 BPM = 4 s.
    let mut d = make(
        EffectKind::TempoDelay,
        sr,
        &[
            ("sync", 1.0),
            ("division", 3.0),
            ("feedback", 0.0),
            ("wet", 1.0),
        ],
    );
    let ctx = ProcessContext {
        sidechain: None,
        tempo_bpm: 30.0,
    };
    let mut warm = vec![0.0; 10_000];
    let mut warm_r = warm.clone();
    d.process_block(&mut warm, &mut warm_r, &ctx);
    let x = impulse(200_000);
    let (mut l, mut r) = (x.clone(), x.clone());
    d.process_block(&mut l, &mut r, &ctx);
    assert_eq!(first_nonzero(&l), Some(192_000));
}

#[test]
fn delay_ping_pong_alternates_channels() {
    let sr = 48_000;
    let mut d = make(
        EffectKind::TempoDelay,
        sr,
        &[
            ("sync", 0.0),
            ("time_ms", 50.0),
            ("feedback", 0.8),
            ("wet", 1.0),
            ("lowpass", 20_000.0),
            ("ping_pong", 1.0),
        ],
    );
    let xl = impulse(20_000);
    let xr = vec![0.0; 20_000];
    let (l, r) = run(d.as_mut(), &xl, &xr, 128);
    let win = |x: &[f32], k: usize| {
        x[k * 2_400..k * 2_400 + 400]
            .iter()
            .map(|v| v.abs())
            .sum::<f32>()
    };
    for k in 1..=4 {
        let (el, er) = (win(&l, k), win(&r, k));
        if k % 2 == 1 {
            assert!(
                el > 0.01 && er < 1e-6,
                "echo {k} must be left: L {el} R {er}"
            );
        } else {
            assert!(
                er > 0.01 && el < 1e-6,
                "echo {k} must be right: L {el} R {er}"
            );
        }
    }
}

#[test]
fn delay_time_change_crossfades() {
    let sr = 48_000;
    let mut d = make(
        EffectKind::TempoDelay,
        sr,
        &[
            ("sync", 0.0),
            ("time_ms", 100.0),
            ("feedback", 0.3),
            ("wet", 1.0),
        ],
    );
    let x = sine(60_000, 220.0, sr);
    let (mut l, mut r) = (x.clone(), x.clone());
    d.process_block(&mut l[..30_000], &mut r[..30_000], &CTX);
    d.set_param(1, 333.0);
    d.process_block(&mut l[30_000..], &mut r[30_000..], &CTX);
    let max_step = l[29_000..]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0, f32::max);
    // A 220 Hz sine of amplitude ≤ ~1.5 changes ≤ ~0.05 per sample; a jump would be ≫.
    assert!(max_step < 0.1, "step {max_step}");
}

// ---------- Reverb ----------

#[test]
fn reverb_impulse_is_bounded_decays_and_ends_in_silence() {
    for sr in [44_100, 48_000, 96_000] {
        let decay = 1.0f32;
        let mut rv = make(
            EffectKind::Reverb,
            sr,
            &[
                ("decay", decay),
                ("predelay", 20.0),
                ("wet", 1.0),
                ("damping", 0.2),
            ],
        );
        let n = (3.0 * sr as f32) as usize;
        let x = impulse(n);
        let (l, r) = run(rv.as_mut(), &x, &x, 512);
        assert!(l.iter().chain(&r).all(|v| v.is_finite() && v.abs() < 2.0));
        // Nothing before the pre-delay.
        let pre = (0.02 * sr as f32) as usize;
        assert!(l[..pre].iter().all(|v| *v == 0.0));
        assert!(
            l[pre..pre + sr as usize / 10]
                .iter()
                .any(|v| v.abs() > 1e-4)
        );
        // RT60 estimate: level drops ~60 dB per `decay` seconds.
        let w = sr as usize / 20;
        let level = |t: f32| {
            let s = (t * sr as f32) as usize;
            db(rms(&l[s..s + w]).max(1e-12))
        };
        let drop_per_s = (level(0.2) - level(0.8)) / 0.6;
        assert!(
            (50.0..=80.0).contains(&drop_per_s),
            "sr {sr}: {drop_per_s} dB/s for RT60 = 1 s"
        );
        // Silence after 2.5 × RT60.
        let tail = &l[(2.5 * sr as f32) as usize..];
        assert!(tail.iter().all(|v| v.abs() < 1e-4));
    }
}

#[test]
fn reverb_long_decay_with_noise_does_not_blow_up() {
    let sr = 48_000;
    let mut rv = make(
        EffectKind::Reverb,
        sr,
        &[("decay", 10.0), ("damping", 0.0), ("wet", 1.0)],
    );
    let x = noise(sr as usize * 4, 5, 1.0);
    let (l, r) = run(rv.as_mut(), &x, &x, 1024);
    let peak = l.iter().chain(&r).fold(0.0f32, |m, v| m.max(v.abs()));
    assert!(peak.is_finite() && peak < 20.0, "peak {peak}");
}

#[test]
fn reverb_decay_change_does_not_allocate_or_jump() {
    let sr = 48_000;
    let mut rv = make(EffectKind::Reverb, sr, &[("decay", 1.0), ("wet", 1.0)]);
    let x = noise(48_000, 9, 0.3);
    let (mut l, mut r) = (x.clone(), x.clone());
    rv.process_block(&mut l[..24_000], &mut r[..24_000], &CTX);
    rv.set_param(0, 8.0);
    rv.set_param(1, 0.9);
    rv.process_block(&mut l[24_000..], &mut r[24_000..], &CTX);
    assert!(l.iter().all(|v| v.is_finite() && v.abs() < 5.0));
}

// ---------- Limiter ----------

#[test]
fn limiter_holds_ceiling_on_transients() {
    for sr in [44_100, 96_000] {
        let mut lim = SafetyLimiter::new(-1.0, 50.0, sr);
        let ceiling = 10f32.powf(-1.0 / 20.0);
        let mut l = noise(50_000, 1, 0.3);
        let mut r = noise(50_000, 2, 0.3);
        // Sudden spikes far over the ceiling.
        for i in (1_000..50_000).step_by(7_919) {
            l[i] = 8.0;
            r[i + 1] = -12.0;
        }
        lim.process(&mut l, &mut r);
        assert!(l.iter().chain(&r).all(|v| v.abs() <= ceiling + 1e-6));
    }
}

#[test]
fn limiter_is_stereo_linked_and_releases() {
    let sr = 48_000;
    let mut lim = SafetyLimiter::new(0.0, 20.0, sr);
    let mut l = vec![2.0; 10];
    let mut r = vec![0.5; 10];
    lim.process(&mut l, &mut r);
    // The same gain on both sides: 0.5 · (1/2).
    assert!((l[5] - 1.0).abs() < 1e-6 && (r[5] - 0.25).abs() < 1e-6);
    assert!(lim.take_min_gain() <= 0.5 + 1e-6);
    // After the peak, gain recovers towards unity with the release time.
    let mut l = vec![0.1; sr as usize / 5];
    let mut r = l.clone();
    lim.process(&mut l, &mut r);
    assert!(l[0] < 0.06, "recovery starts from the reduced gain");
    assert!(
        (l[l.len() - 1] - 0.1).abs() < 1e-3,
        "recovered after 10 × release"
    );
}

// ---------- All processors ----------

#[test]
fn all_effects_block_size_independent_and_finite() {
    let overrides: &[(&str, f32)] = &[
        ("sync", 0.0),
        ("time_ms", 37.0),
        ("wet", 0.7),
        ("mid1_gain", 9.0),
    ];
    for kind in ALL {
        for sr in [44_100, 48_000, 96_000] {
            let filtered: Vec<(&str, f32)> = overrides
                .iter()
                .copied()
                .filter(|(id, _)| kind.param(id).is_some())
                .collect();
            let x = noise(30_000, 13, 0.7);
            let y = noise(30_000, 17, 0.7);
            let mut a = make(kind, sr, &filtered);
            let (ref_l, ref_r) = run(a.as_mut(), &x, &y, 30_000);
            assert!(
                ref_l.iter().chain(&ref_r).all(|v| v.is_finite()),
                "{kind:?} {sr}"
            );
            for block in [1, 7, 64, 1_000] {
                let mut b = make(kind, sr, &filtered);
                let (l, r) = run(b.as_mut(), &x, &y, block);
                assert_eq!(l, ref_l, "{kind:?} sr {sr} block {block}");
                assert_eq!(r, ref_r, "{kind:?} sr {sr} block {block}");
            }
        }
    }
}

#[test]
fn all_effects_reset_to_silence() {
    for kind in ALL {
        let mut p = make(
            kind,
            48_000,
            &[
                ("wet", 1.0),
                ("sync", 0.0),
                ("time_ms", 100.0),
                ("feedback", 0.9),
            ],
        );
        let x = noise(48_000, 21, 0.9);
        run(p.as_mut(), &x, &x, 512);
        // Bounded steps until clean.
        let mut steps = 0;
        while !p.reset_step(16_384) {
            steps += 1;
            assert!(steps < 1_000, "{kind:?} reset never finishes");
        }
        let silence = vec![0.0; 48_000];
        let (l, r) = run(p.as_mut(), &silence, &silence, 512);
        assert!(
            l.iter().chain(&r).all(|v| *v == 0.0),
            "{kind:?} not silent after reset"
        );
    }
}

#[test]
fn parameter_events_apply_at_frame_offsets() {
    let sr = 48_000;
    let mut a = make(EffectKind::TempoDelay, sr, &[("sync", 0.0), ("wet", 0.0)]);
    let mut b = make(EffectKind::TempoDelay, sr, &[("sync", 0.0), ("wet", 0.0)]);
    let x = noise(4_096, 4, 0.5);
    // Event at frame 1000 ≡ splitting the block there and setting the param.
    let events = [ParamEvent {
        frame: 1_000,
        index: 4,
        value: 1.0,
    }];
    let (mut l1, mut r1) = (x.clone(), x.clone());
    process_with_events(a.as_mut(), &mut l1, &mut r1, &events, &CTX);
    let (mut l2, mut r2) = (x.clone(), x.clone());
    b.process_block(&mut l2[..1_000], &mut r2[..1_000], &CTX);
    b.set_param(4, 1.0);
    b.process_block(&mut l2[1_000..], &mut r2[1_000..], &CTX);
    assert_eq!(l1, l2);
}
