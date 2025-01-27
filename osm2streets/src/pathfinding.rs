use geom::Distance;
use petgraph::graphmap::DiGraphMap;

use crate::{osm, Direction, LaneType, OriginalRoad, StreetNetwork};

// A/B Street's map_model has lots of pathfinding support at both a road segment and lane level.
// This is a delibrately simple subset of functionality for now.

impl StreetNetwork {
    /// Calculates a rough driving distance between intersections, excluding the turning movement
    /// through intersections.
    pub fn path_dist_to(&self, from: osm::NodeID, to: osm::NodeID) -> Option<Distance> {
        let mut graph = DiGraphMap::new();
        for (id, r) in &self.roads {
            graph.add_edge(id.i1, id.i2, id);
            if r.oneway_for_driving().is_none() {
                graph.add_edge(id.i2, id.i1, id);
            }
        }
        petgraph::algo::dijkstra(&graph, from, Some(to), |(_, _, r)| {
            // TODO Expensive!
            self.roads[r].length()
        })
        .get(&to)
        .cloned()
    }

    /// Calculates a driving path between intersections. The result says which direction to cross
    /// each road.
    pub fn simple_path(
        &self,
        from: osm::NodeID,
        to: osm::NodeID,
        lane_types: &[LaneType],
    ) -> Option<Vec<(OriginalRoad, Direction)>> {
        let mut graph = DiGraphMap::new();
        for (id, r) in &self.roads {
            let mut fwd = false;
            let mut back = false;
            for lane in &r.lane_specs_ltr {
                if lane_types.contains(&lane.lt) {
                    if lane.dir == Direction::Fwd {
                        fwd = true;
                    } else {
                        back = true;
                    }
                }
            }
            if fwd {
                graph.add_edge(id.i1, id.i2, (*id, Direction::Fwd));
            }
            if back {
                graph.add_edge(id.i2, id.i1, (*id, Direction::Back));
            }
        }
        let (_, path) = petgraph::algo::astar(
            &graph,
            from,
            |i| i == to,
            // TODO Expensive!
            |(_, _, (r, _))| self.roads[r].length(),
            |_| Distance::ZERO,
        )?;
        let roads: Vec<(OriginalRoad, Direction)> = path
            .windows(2)
            .map(|pair| *graph.edge_weight(pair[0], pair[1]).unwrap())
            .collect();
        Some(roads)
    }
}
