// name: Humanize
// description: Small random variations of timing, length and velocity

fn params() {
    [
        #{ key: "timing", label: "Start ± (ticks)", kind: "int", min: 0, max: 480, init: 10 },
        #{ key: "length", label: "Length ± (ticks)", kind: "int", min: 0, max: 480, init: 10 },
        #{ key: "velocity", label: "Velocity ±", kind: "int", min: 0, max: 64, init: 8 },
        #{ key: "pitch", label: "Pitch ± (semitones)", kind: "int", min: 0, max: 12, init: 0 },
        #{ key: "keep_chords", label: "Keep chords together", kind: "bool", init: true },
    ]
}

fn clamp(x, lo, hi) {
    if x < lo { lo } else if x > hi { hi } else { x }
}

fn run(ctx) {
    let p = ctx.params;
    let out = [];
    let last_start = -1;
    let offset = 0;
    for n in ctx.scope {
        // One timing offset per chord when chords stay together.
        if !p.keep_chords || n.start != last_start {
            offset = random_int(-p.timing, p.timing);
            last_start = n.start;
        }
        let s = clamp(n.start + offset, 0, ctx.pattern_len - 1);
        let l = n.length + random_int(-p.length, p.length);
        if l < 1 { l = 1; }
        let v = clamp(n.velocity + random_int(-p.velocity, p.velocity), 1, 127);
        let k = clamp(n.key + random_int(-p.pitch, p.pitch), 0, 127);
        out.push(#{ id: n.id, start: s, length: l, key: k, velocity: v });
    }
    #{ notes: out }
}
