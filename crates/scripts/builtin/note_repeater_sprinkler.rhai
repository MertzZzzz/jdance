// name: Note repeater sprinkler
// description: Cuts notes into repeats and keeps each with a probability

fn params() {
    [
        #{ key: "step", label: "Step (ticks)", kind: "int", min: 1, max: 3840, init: 120 },
        #{ key: "probability", label: "Probability", kind: "float", min: 0.0, max: 1.0, init: 0.7 },
        #{ key: "gate", label: "Gate %", kind: "int", min: 1, max: 100, init: 100 },
        #{ key: "variation", label: "Velocity ±", kind: "int", min: 0, max: 64, init: 0 },
    ]
}

fn run(ctx) {
    let p = ctx.params;
    let out = [];
    for n in ctx.scope {
        let end = n.start + n.length;
        let c = n.start;
        let first = true;
        while c < end {
            let cell_end = if c + p.step > end { end } else { c + p.step };
            let len = ((cell_end - c) * p.gate * 2 + 100) / 200;
            if len < 1 { len = 1; }
            if random() < p.probability {
                let v = n.velocity;
                if p.variation > 0 {
                    v += random_int(-p.variation, p.variation);
                }
                if v < 1 { v = 1; }
                if v > 127 { v = 127; }
                let id = if first { n.id } else { 0 };
                out.push(#{ id: id, start: c, length: len, key: n.key, velocity: v });
            }
            first = false;
            c = cell_end;
        }
    }
    #{ notes: out }
}
