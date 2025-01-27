use std::iter;

use abstutil::Tags;
use geom::Distance;

use crate::{osm, BufferType, Direction, DrivingSide, LaneSpec, LaneType, MapConfig};

/// Purely from OSM tags, determine the lanes that a road segment has. This is the "classic"
/// implementation -- the default, but on its way out.
pub fn get_lane_specs_ltr(tags: &Tags, cfg: &MapConfig) -> Vec<LaneSpec> {
    if cfg.osm2lanes {
        return super::osm2lanes::get_lane_specs_ltr_experimental(tags, cfg);
    }

    // TODO This hides a potentially expensive (on a hot-path) clone
    let mut tags = tags.clone();
    // This'll do weird things for the special cases of railways and cycleways/footways, but the
    // added tags will be ignored, so it doesn't matter too much. Running this later causes borrow
    // checker problems.
    infer_sidewalk_tags(&mut tags, cfg);

    let fwd = |lt: LaneType| LaneSpec {
        lt,
        dir: Direction::Fwd,
        width: LaneSpec::typical_lane_widths(lt, &tags)[0].0,
    };
    let back = |lt: LaneType| LaneSpec {
        lt,
        dir: Direction::Back,
        width: LaneSpec::typical_lane_widths(lt, &tags)[0].0,
    };

    // Easy special cases first.
    if tags.is_any("railway", vec!["light_rail", "rail"]) {
        return vec![fwd(LaneType::LightRail)];
    }

    // If it's a primarily cycleway, have directional bike lanes and add a shoulder for walking
    // TODO Consider variations of SharedUse that specify the priority is cyclists over pedestrians
    // in this case?
    if tags.is(osm::HIGHWAY, "cycleway") {
        let mut fwd_side = vec![fwd(LaneType::Biking)];
        let mut back_side = if tags.is("oneway", "yes") {
            vec![]
        } else {
            vec![back(LaneType::Biking)]
        };

        // TODO If this cycleway is parallel to a main road, we might end up with double sidewalks.
        // Once snapping works well, this problem will improve
        if !tags.is("foot", "no") {
            fwd_side.push(fwd(LaneType::Shoulder));
            if !back_side.is_empty() {
                back_side.push(back(LaneType::Shoulder));
            }
        }
        return LaneSpec::assemble_ltr(fwd_side, back_side, cfg.driving_side);
    }

    // These roads will only exist if cfg.inferred_sidewalks is false
    if tags.is(osm::HIGHWAY, "footway") && tags.is_any("footway", vec!["crossing", "sidewalk"]) {
        // Treating a crossing as a sidewalk for now. Eventually crossings need to be dealt with
        // completely differently.
        return vec![fwd(LaneType::Sidewalk)];
    }

    // Handle pedestrian-oriented spaces
    if tags.is_any(
        osm::HIGHWAY,
        vec!["footway", "path", "pedestrian", "steps", "track"],
    ) {
        // Assume no bikes unless they're explicitly allowed
        if tags.is_any("bicycle", vec!["designated", "yes", "dismount"]) {
            return vec![fwd(LaneType::SharedUse)];
        }

        return vec![fwd(LaneType::Footway)];
    }

    // Most cases are below -- it's a "normal road"

    // TODO Reversible roads should be handled differently?
    let oneway =
        tags.is_any("oneway", vec!["yes", "reversible"]) || tags.is("junction", "roundabout");

    // How many driving lanes in each direction?
    let num_driving_fwd = if let Some(n) = tags
        .get("lanes:forward")
        .and_then(|num| num.parse::<usize>().ok())
    {
        n
    } else if let Some(n) = tags.get("lanes").and_then(|num| num.parse::<usize>().ok()) {
        if oneway {
            n
        } else if n % 2 == 0 {
            n / 2
        } else {
            // usize division rounds down
            (n / 2) + 1
        }
    } else {
        1
    };
    let num_driving_back = if let Some(n) = tags
        .get("lanes:backward")
        .and_then(|num| num.parse::<usize>().ok())
    {
        n
    } else if let Some(n) = tags.get("lanes").and_then(|num| num.parse::<usize>().ok()) {
        let base = n - num_driving_fwd;
        if oneway {
            base
        } else {
            // lanes=1 but not oneway... what is this supposed to mean?
            base.max(1)
        }
    } else if oneway {
        0
    } else {
        1
    };

    #[allow(clippy::if_same_then_else)] // better readability
    let driving_lane = if tags.is("access", "no")
        && (tags.is("bus", "yes") || tags.is_any("psv", vec!["yes", "designated"]))
    {
        LaneType::Bus
    } else if tags
        .get("motor_vehicle:conditional")
        .map(|x| x.starts_with("no"))
        .unwrap_or(false)
        && tags.is("bus", "yes")
    {
        // Example: 3rd Ave in downtown Seattle
        LaneType::Bus
    } else if tags.is("access", "no") || tags.is("highway", "construction") {
        LaneType::Construction
    } else {
        LaneType::Driving
    };

    // These are ordered from the road center, going outwards. Most of the members of fwd_side will
    // have Direction::Fwd, but there can be exceptions with two-way cycletracks.
    let mut fwd_side: Vec<LaneSpec> = iter::repeat_with(|| fwd(driving_lane))
        .take(num_driving_fwd)
        .collect();
    let mut back_side: Vec<LaneSpec> = iter::repeat_with(|| back(driving_lane))
        .take(num_driving_back)
        .collect();
    // TODO Fix upstream. https://wiki.openstreetmap.org/wiki/Key:centre_turn_lane
    if tags.is("lanes:both_ways", "1") || tags.is("centre_turn_lane", "yes") {
        fwd_side.insert(0, fwd(LaneType::SharedLeftTurn));
    }

    if driving_lane == LaneType::Construction {
        return LaneSpec::assemble_ltr(fwd_side, back_side, cfg.driving_side);
    }

    let fwd_bus_spec = if let Some(s) = tags.get("bus:lanes:forward") {
        s
    } else if let Some(s) = tags.get("psv:lanes:forward") {
        s
    } else if oneway {
        if let Some(s) = tags.get("bus:lanes") {
            s
        } else if let Some(s) = tags.get("psv:lanes") {
            s
        } else {
            ""
        }
    } else {
        ""
    };
    if !fwd_bus_spec.is_empty() {
        let parts: Vec<&str> = fwd_bus_spec.split('|').collect();
        let offset = if fwd_side[0].lt == LaneType::SharedLeftTurn {
            1
        } else {
            0
        };
        if parts.len() == fwd_side.len() - offset {
            for (idx, part) in parts.into_iter().enumerate() {
                if part == "designated" {
                    fwd_side[idx + offset].lt = LaneType::Bus;
                }
            }
        }
    }
    if let Some(spec) = tags
        .get("bus:lanes:backward")
        .or_else(|| tags.get("psv:lanes:backward"))
    {
        let parts: Vec<&str> = spec.split('|').collect();
        if parts.len() == back_side.len() {
            for (idx, part) in parts.into_iter().enumerate() {
                if part == "designated" {
                    back_side[idx].lt = LaneType::Bus;
                }
            }
        }
    }

    if tags.is_any("cycleway", vec!["lane", "track"]) {
        fwd_side.push(fwd(LaneType::Biking));
        if !back_side.is_empty() {
            back_side.push(back(LaneType::Biking));
        }
    } else if tags.is_any("cycleway:both", vec!["lane", "track"]) {
        fwd_side.push(fwd(LaneType::Biking));
        back_side.push(back(LaneType::Biking));
    } else {
        // Note here that we look at driving_side frequently, to match up left/right with fwd/back.
        // If we're driving on the right, then right=fwd. Driving on the left, then right=back.
        //
        // TODO Can we express this more simply by referring to a left_side and right_side here?
        if tags.is_any("cycleway:right", vec!["lane", "track"]) {
            if cfg.driving_side == DrivingSide::Right {
                if tags.is("cycleway:right:oneway", "no") || tags.is("oneway:bicycle", "no") {
                    fwd_side.push(back(LaneType::Biking));
                }
                fwd_side.push(fwd(LaneType::Biking));
            } else {
                if tags.is("cycleway:right:oneway", "no") || tags.is("oneway:bicycle", "no") {
                    back_side.push(fwd(LaneType::Biking));
                }
                back_side.push(back(LaneType::Biking));
            }
        }
        if tags.is("cycleway:left", "opposite_lane") || tags.is("cycleway", "opposite_lane") {
            if cfg.driving_side == DrivingSide::Right {
                back_side.push(back(LaneType::Biking));
            } else {
                fwd_side.push(fwd(LaneType::Biking));
            }
        }
        if tags.is_any("cycleway:left", vec!["lane", "opposite_track", "track"]) {
            if cfg.driving_side == DrivingSide::Right {
                if tags.is("cycleway:left:oneway", "no") || tags.is("oneway:bicycle", "no") {
                    back_side.push(fwd(LaneType::Biking));
                    back_side.push(back(LaneType::Biking));
                } else if oneway {
                    fwd_side.insert(0, fwd(LaneType::Biking));
                } else {
                    back_side.push(back(LaneType::Biking));
                }
            } else {
                // TODO This should mimic the logic for right-handed driving, but I need test cases
                // first to do this sanely
                if tags.is("cycleway:left:oneway", "no") || tags.is("oneway:bicycle", "no") {
                    fwd_side.push(back(LaneType::Biking));
                }
                fwd_side.push(fwd(LaneType::Biking));
            }
        }
    }

    // My brain hurts. How does the above combinatorial explosion play with
    // https://wiki.openstreetmap.org/wiki/Proposed_features/cycleway:separation? Let's take the
    // "post-processing" approach.
    // TODO Not attempting left-handed driving yet.
    // TODO A two-way cycletrack on one side of a one-way road will almost definitely break this.
    if let Some(buffer) = tags
        .get("cycleway:right:separation:left")
        .and_then(osm_separation_type)
    {
        // TODO These shouldn't fail, but snapping is imperfect... like around
        // https://www.openstreetmap.org/way/486283205
        if let Some(idx) = fwd_side.iter().position(|x| x.lt == LaneType::Biking) {
            fwd_side.insert(idx, fwd(LaneType::Buffer(buffer)));
        }
    }
    if let Some(buffer) = tags
        .get("cycleway:left:separation:left")
        .and_then(osm_separation_type)
    {
        if let Some(idx) = back_side.iter().position(|x| x.lt == LaneType::Biking) {
            back_side.insert(idx, back(LaneType::Buffer(buffer)));
        }
    }
    if let Some(buffer) = tags
        .get("cycleway:left:separation:right")
        .and_then(osm_separation_type)
    {
        // This is assuming a one-way road. That's why we're not looking at back_side.
        if let Some(idx) = fwd_side.iter().position(|x| x.lt == LaneType::Biking) {
            fwd_side.insert(idx + 1, fwd(LaneType::Buffer(buffer)));
        }
    }

    if driving_lane == LaneType::Driving {
        let has_parking = vec!["parallel", "diagonal", "perpendicular"];
        let parking_lane_fwd = tags.is_any(osm::PARKING_RIGHT, has_parking.clone())
            || tags.is_any(osm::PARKING_BOTH, has_parking.clone());
        let parking_lane_back = tags.is_any(osm::PARKING_LEFT, has_parking.clone())
            || tags.is_any(osm::PARKING_BOTH, has_parking);
        if parking_lane_fwd {
            fwd_side.push(fwd(LaneType::Parking));
        }
        if parking_lane_back {
            back_side.push(back(LaneType::Parking));
        }
    }

    if tags.is(osm::SIDEWALK, "both") {
        fwd_side.push(fwd(LaneType::Sidewalk));
        back_side.push(back(LaneType::Sidewalk));
    } else if tags.is(osm::SIDEWALK, "separate") && cfg.inferred_sidewalks {
        // TODO Need to snap separate sidewalks to ways. Until then, just do this.
        fwd_side.push(fwd(LaneType::Sidewalk));
        if !back_side.is_empty() {
            back_side.push(back(LaneType::Sidewalk));
        }
    } else if tags.is(osm::SIDEWALK, "right") {
        if cfg.driving_side == DrivingSide::Right {
            fwd_side.push(fwd(LaneType::Sidewalk));
        } else {
            back_side.push(back(LaneType::Sidewalk));
        }
    } else if tags.is(osm::SIDEWALK, "left") {
        if cfg.driving_side == DrivingSide::Right {
            back_side.push(back(LaneType::Sidewalk));
        } else {
            fwd_side.push(fwd(LaneType::Sidewalk));
        }
    }

    // Playing fast-and-loose here (and not checking the lane being modified is a sidewalk) because
    // of imminent cutover to osm2lanes, where this will be done way more carefully
    if let Some(x) = tags
        .get("sidewalk:left:width")
        .and_then(|num| num.parse::<f64>().ok())
    {
        if cfg.driving_side == DrivingSide::Right {
            back_side.last_mut().unwrap().width = Distance::meters(x);
        } else {
            fwd_side.last_mut().unwrap().width = Distance::meters(x);
        }
    }
    if let Some(x) = tags
        .get("sidewalk:right:width")
        .and_then(|num| num.parse::<f64>().ok())
    {
        if cfg.driving_side == DrivingSide::Right {
            fwd_side.last_mut().unwrap().width = Distance::meters(x);
        } else {
            back_side.last_mut().unwrap().width = Distance::meters(x);
        }
    }

    let mut need_fwd_shoulder = fwd_side
        .last()
        .map(|spec| spec.lt != LaneType::Sidewalk)
        .unwrap_or(true);
    let mut need_back_shoulder = back_side
        .last()
        .map(|spec| spec.lt != LaneType::Sidewalk)
        .unwrap_or(true);
    if tags.is_any(
        osm::HIGHWAY,
        vec!["motorway", "motorway_link", "construction"],
    ) || tags.is("foot", "no")
        || tags.is("access", "no")
        || tags.is("motorroad", "yes")
    {
        need_fwd_shoulder = false;
        need_back_shoulder = false;
    }
    // If it's a one-way, fine to not have sidewalks on both sides.
    if tags.is("oneway", "yes") {
        need_back_shoulder = false;
    }

    // For living streets in Krakow, there aren't separate footways. People can walk in the street.
    // For now, model that by putting shoulders.
    if cfg.inferred_sidewalks || tags.is(osm::HIGHWAY, "living_street") {
        if need_fwd_shoulder {
            fwd_side.push(fwd(LaneType::Shoulder));
        }
        if need_back_shoulder {
            back_side.push(back(LaneType::Shoulder));
        }
    }

    LaneSpec::assemble_ltr(fwd_side, back_side, cfg.driving_side)
}

// See https://wiki.openstreetmap.org/wiki/Proposed_features/cycleway:separation#Typical_values.
// Lots of these mappings are pretty wacky right now. We need more BufferTypes.
#[allow(clippy::ptr_arg)] // Can't chain with `tags.get("foo").and_then` otherwise
fn osm_separation_type(x: &String) -> Option<BufferType> {
    match x.as_ref() {
        "bollard" | "vertical_panel" => Some(BufferType::FlexPosts),
        "kerb" | "separation_kerb" => Some(BufferType::Curb),
        "grass_verge" | "planter" | "tree_row" => Some(BufferType::Planters),
        "guard_rail" | "jersey_barrier" | "railing" => Some(BufferType::JerseyBarrier),
        // TODO Make sure there's a parking lane on that side... also mapped? Any flex posts in
        // between?
        "parking_lane" => None,
        "barred_area" | "dashed_line" | "solid_line" => Some(BufferType::Stripes),
        _ => None,
    }
}

// If sidewalks aren't explicitly tagged on a road, fill them in. This only happens when the map is
// configured to have this inference.
pub(crate) fn infer_sidewalk_tags(tags: &mut Tags, cfg: &MapConfig) {
    if tags.contains_key(osm::SIDEWALK) || !cfg.inferred_sidewalks {
        return;
    }

    if tags.contains_key("sidewalk:left") || tags.contains_key("sidewalk:right") {
        // Attempt to mangle
        // https://wiki.openstreetmap.org/wiki/Key:sidewalk#Separately_mapped_sidewalks_on_only_one_side
        // into left/right/both. We have to make assumptions for missing values.
        let right = !tags.is("sidewalk:right", "no");
        let left = !tags.is("sidewalk:left", "no");
        let value = match (right, left) {
            (true, true) => "both",
            (true, false) => "right",
            (false, true) => "left",
            (false, false) => "none",
        };
        tags.insert(osm::SIDEWALK, value);
    } else if tags.is_any(osm::HIGHWAY, vec!["motorway", "motorway_link"])
        || tags.is_any("junction", vec!["intersection", "roundabout"])
        || tags.is("foot", "no")
        || tags.is(osm::HIGHWAY, "service")
        || tags.is_any(osm::HIGHWAY, vec!["cycleway", "pedestrian", "track"])
    {
        tags.insert(osm::SIDEWALK, "none");
    } else if tags.is("oneway", "yes") {
        if cfg.driving_side == DrivingSide::Right {
            tags.insert(osm::SIDEWALK, "right");
        } else {
            tags.insert(osm::SIDEWALK, "left");
        }
        if tags.is_any(osm::HIGHWAY, vec!["residential", "living_street"])
            && !tags.is("dual_carriageway", "yes")
        {
            tags.insert(osm::SIDEWALK, "both");
        }
    } else {
        tags.insert(osm::SIDEWALK, "both");
    }
}
