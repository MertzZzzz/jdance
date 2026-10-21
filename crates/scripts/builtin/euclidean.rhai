// name: Euclidean
// description: Evenly spread hits (Bjorklund) over the range, repeated

fn params() {
    [
        #{ key: "steps", label: "Steps", kind: "int", min: 1, max: 128, init: 16 },
        #{ key: "pulses", label: "Pulses", kind: "int", min: 0, max: 128, init: 5 },
        #{ key: "rotation", label: "Rotation", kind: "int", min: 0, max: 127, init: 0 },
        #{ key: "step", label: "Step (ticks)", kind: "int", min: 1, max: 3840, init: 240 },
        #{ key: "key", label: "Key", kind: "int", min: 0, max: 127, init: 60 },
        #{ key: "velocity", label: "Velocity", kind: "int", min: 1, max: 127, init: 100 },
        #{ key: "gate", label: "Gate %", kind: "int", min: 1, max: 100, init: 50 },
        #{ key: "insertion", label: "Insert", kind: "choice",
           choices: ["replace range", "add a layer"], init: 0 },
    ]
}

fn run(ctx) {
    let p = ctx.params;
    let steps = p.steps;
    let pulses = if p.pulses > steps { steps } else { p.pulses };
    let hits = bjorklund(steps, pulses);
    let len = (p.step * p.gate * 2 + 100) / 200;
    if len < 1 { len = 1; }
    let out = [];
    let t = ctx.range_start;
    let i = 0;
    while t < ctx.range_end {
        // Rotation moves the pattern later by whole steps.
        let k = ((i - p.rotation) % steps + steps) % steps;
        if hits[k] {
            let l = if t + len > ctx.range_end { ctx.range_end - t } else { len };
            out.push(#{ start: t, length: l, key: p.key, velocity: p.velocity });
        }
        t += p.step;
        i += 1;
    }
    let mode = if p.insertion == "add a layer" { "append" } else { "range" };
    #{ notes: out, mode: mode }
}
