//! The schedule census (`WOW_SCHED_CENSUS=1`): the structural inventory behind the 1435 band
//! map's orchestration rows — per schedule, every system with its executor-relevant flags
//! (non-`Send`, exclusive, has-deferred), for BOTH worlds. Born with decision 1437: the parked
//! frame pays ~5.4 ms/f of pure scheduling (schedule selves + executor + empty command applies
//! across ~600 executed systems), and neither consolidation nor executor choices can be argued
//! about until the population has names and counts.
//!
//! Why runtime one-shots instead of a pre-run `Schedule::initialize` walk: bevy removes a
//! schedule from the `Schedules` resource WHILE it runs (`World::schedule_scope`), so no single
//! vantage sees everything — and pre-run initialization is the wrong tool twice over
//! (`Extract` params panic without the runtime-only `MainWorld`, and `Local<impl FromWorld>`
//! state may read resources Startup hasn't inserted yet). Two vantages per world cover each
//! other: `Update` and `PostUpdate` in the main world, `ExtractSchedule` and `Render` in the
//! render app. Only the `Main`/`RenderStartup` runners stay invisible (each is the fixed
//! one-system runner bevy ships).

use bevy::prelude::*;
use bevy::render::{ExtractSchedule, Render, RenderApp};

/// Frames to wait before dumping — deep enough that every schedule has run at least once (the
/// census reads the live graph, not what a first frame happens to have reached).
const CENSUS_FRAME: u32 = 10;

/// Frames to wait before the census run exits — the render app runs a frame behind the main
/// world (pipelined), so give its vantages room past [`CENSUS_FRAME`] before the app closes.
const EXIT_FRAME: u32 = 40;

/// The census instrument. Purely structural — it does not need the world, a server, or a
/// character: the schedule graph is fixed once the plugins have built, so a login-screen run
/// answers for every state.
pub(crate) struct SchedCensusPlugin;

impl Plugin for SchedCensusPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DumpedSchedules>()
            .add_systems(Update, census_vantage("main", "Update"))
            .add_systems(PostUpdate, census_vantage("main", "PostUpdate"))
            .add_systems(Last, census_exit);
        // The render app still lives in the main `App` here (pipelining detaches it at cleanup,
        // after every plugin has built). Its two vantages mirror the main world's pair.
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .init_resource::<DumpedSchedules>()
                .add_systems(ExtractSchedule, census_vantage("render", "Extract"))
                .add_systems(Render, census_vantage("render", "Render"));
        }
    }
}

/// Which schedules this world's earlier vantage already printed — the later vantage adds only
/// what the earlier one could not see (itself, chiefly).
#[derive(Resource, Default)]
struct DumpedSchedules(std::collections::HashSet<String>);

/// One vantage: an exclusive one-shot that, at [`CENSUS_FRAME`], prints every schedule visible
/// in `Schedules` that no earlier vantage in this world has printed.
fn census_vantage(
    world_tag: &'static str,
    vantage: &'static str,
) -> impl FnMut(&mut World, Local<u32>) {
    move |world: &mut World, mut frame: Local<u32>| {
        *frame += 1;
        if *frame != CENSUS_FRAME {
            return;
        }
        world.resource_scope::<DumpedSchedules, _>(|world, mut dumped| {
            world.resource_scope::<Schedules, _>(|_, schedules| {
                let mut labels: Vec<(String, &bevy::ecs::schedule::Schedule)> = schedules
                    .iter()
                    .map(|(label, sched)| (format!("{label:?}"), sched))
                    .filter(|(name, _)| !dumped.0.contains(name))
                    .collect();
                labels.sort_by(|a, b| a.0.cmp(&b.0));
                for (name, sched) in labels {
                    dump_schedule(world_tag, vantage, &name, sched);
                    dumped.0.insert(name);
                }
            });
        });
    }
}

/// Print one schedule: the tally line, then one line per system. `systems()` errors only on an
/// uninitialized graph — impossible by [`CENSUS_FRAME`] for a schedule that runs, and a
/// registered-but-never-run schedule is exactly worth flagging as such.
fn dump_schedule(
    world_tag: &str,
    vantage: &str,
    name: &str,
    sched: &bevy::ecs::schedule::Schedule,
) {
    let Ok(systems) = sched.systems() else {
        println!(
            "SCHED_CENSUS world={world_tag} vantage={vantage} schedule={name} \
             n={} uninitialized=1",
            sched.systems_len()
        );
        return;
    };
    let (mut n, mut nonsend, mut exclusive, mut deferred) = (0u32, 0u32, 0u32, 0u32);
    let mut rows: Vec<String> = Vec::new();
    for (_, sys) in systems {
        n += 1;
        let mut flags = String::new();
        if !sys.is_send() {
            nonsend += 1;
            flags.push('N');
        }
        if sys.is_exclusive() {
            exclusive += 1;
            flags.push('X');
        }
        if sys.has_deferred() {
            deferred += 1;
            flags.push('D');
        }
        rows.push(format!(
            "SCHED_SYS world={world_tag} schedule={name} flags={} name={}",
            if flags.is_empty() { "-".into() } else { flags },
            sys.name()
        ));
    }
    println!(
        "SCHED_CENSUS world={world_tag} vantage={vantage} schedule={name} \
         n={n} nonsend={nonsend} exclusive={exclusive} deferred={deferred}"
    );
    for row in rows {
        println!("{row}");
    }
}

/// Close the run once both worlds have had time to print — the census is its own whole run.
fn census_exit(mut frame: Local<u32>, mut exit: MessageWriter<AppExit>) {
    *frame += 1;
    if *frame == EXIT_FRAME {
        println!("SCHED_CENSUS_DONE");
        exit.write(AppExit::Success);
    }
}
