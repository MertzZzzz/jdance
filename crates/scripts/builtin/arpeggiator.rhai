// name: Arpeggiator
// description: Plays each chord as an arpeggio (the same as Tools > Arpeggiate)

fn params() {
    [
        #{ key: "direction", label: "Direction", kind: "choice",
           choices: ["up", "down", "up-down", "down-up", "random"], init: 0 },
        #{ key: "step", label: "Step (ticks)", kind: "int", min: 1, max: 3840, init: 240 },
        #{ key: "gate", label: "Gate %", kind: "int", min: 1, max: 100, init: 90 },
        #{ key: "octaves", label: "Octaves", kind: "int", min: 1, max: 4, init: 1 },
    ]
}

fn run(ctx) {
    let p = ctx.params;
    #{ notes: arpeggiate(p.direction, p.step, p.gate, p.octaves) }
}
