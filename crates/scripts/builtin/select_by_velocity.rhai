// name: Select by velocity and more
// description: Selects notes by velocity, key, length and start
// result: selection

fn params() {
    [
        #{ key: "use_velocity", label: "Filter velocity", kind: "bool", init: true },
        #{ key: "velocity_min", label: "Velocity from", kind: "int", min: 1, max: 127, init: 1 },
        #{ key: "velocity_max", label: "Velocity to", kind: "int", min: 1, max: 127, init: 127 },
        #{ key: "use_key", label: "Filter key", kind: "bool", init: false },
        #{ key: "key_min", label: "Key from", kind: "int", min: 0, max: 127, init: 0 },
        #{ key: "key_max", label: "Key to", kind: "int", min: 0, max: 127, init: 127 },
        #{ key: "use_length", label: "Filter length", kind: "bool", init: false },
        #{ key: "length_min", label: "Length from (ticks)", kind: "int", min: 1, max: 1000000, init: 1 },
        #{ key: "length_max", label: "Length to (ticks)", kind: "int", min: 1, max: 1000000, init: 1000000 },
        #{ key: "use_start", label: "Filter start", kind: "bool", init: false },
        #{ key: "start_min", label: "Start from (ticks)", kind: "int", min: 0, max: 1000000, init: 0 },
        #{ key: "start_max", label: "Start to (ticks)", kind: "int", min: 0, max: 1000000, init: 1000000 },
        #{ key: "mode", label: "Selection", kind: "choice",
           choices: ["replace", "add", "subtract", "intersect"], init: 0 },
        #{ key: "within_selection", label: "Only look at selected notes", kind: "bool", init: false },
    ]
}

fn matches(n, p) {
    (!p.use_velocity || (n.velocity >= p.velocity_min && n.velocity <= p.velocity_max))
        && (!p.use_key || (n.key >= p.key_min && n.key <= p.key_max))
        && (!p.use_length || (n.length >= p.length_min && n.length <= p.length_max))
        && (!p.use_start || (n.start >= p.start_min && n.start <= p.start_max))
}

fn run(ctx) {
    let p = ctx.params;
    let out = [];
    for n in ctx.notes {
        let candidate = !p.within_selection || n.selected;
        let hit = candidate && matches(n, p);
        let keep = switch p.mode {
            "add" => n.selected || hit,
            "subtract" => n.selected && !hit,
            "intersect" => n.selected && hit,
            _ => hit,
        };
        if keep { out.push(n.id); }
    }
    #{ select: out }
}
