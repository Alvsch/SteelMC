/// Holds the total amount of experience points a player has
#[derive(Default, Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Experience {
    total_points: i64, // cannot be u32
}

impl Experience {
    /// A new Experience state with `total_points`
    #[must_use]
    pub const fn new(total_points: i64) -> Self {
        Self { total_points }
    }

    /// Points required to go from `level` to `level + 1`.
    #[must_use]
    pub const fn points_for_level(level: i64) -> i64 {
        match level {
            0..15 => 2 * level + 7,
            15..30 => 37 + (level - 15) * 5,
            _ => 9 * (level - 30) + 112,
        }
    }

    /// Calculates the total cumulative points at a specific level
    #[must_use]
    pub const fn total_points_at_level(level: i64) -> i64 {
        match level {
            0..=15 => level * level + 6 * level,
            16..=30 => 360 + ((level * (5 * level - 81)) / 2),
            _ => (level * (level * 9 - 325) / 2) + 2220,
        }
    }

    /// Calculates the current level from the total cumulative points
    #[must_use]
    pub fn level(self) -> i64 {
        let points = self.total_points as f64;
        if (0.0..=315.0).contains(&points) {
            return f64::midpoint(-6.0, f64::sqrt(36.0 + 4.0 * points)) as i64;
        } else if (316.0..=1507.0).contains(&points) {
            return ((40.5 + f64::sqrt(-1959.75 + 10.0 * points)) / 5.0) as i64;
        }
        ((162.5 + f64::sqrt(-13553.75 + 18.0 * points)) / 9.0) as i64
    }

    /// The points of the player to the next level
    #[must_use]
    pub fn points(self) -> i64 {
        self.total_points - Self::total_points_at_level(self.level())
    }

    /// Returns the progress to the next level between 0.0 and 1.0
    #[must_use]
    pub fn progress(self) -> f64 {
        let level = self.level();

        (self.total_points - Self::total_points_at_level(level)) as f64
            / (Self::points_for_level(level + 1)) as f64
    }

    /// Add levels to the total experience
    pub fn add_levels(&mut self, additional_levels: i64) {
        let progress = self.progress();
        let new_level = self.level().saturating_add(additional_levels);
        self.total_points = Self::total_points_at_level(new_level)
            + (progress * Self::points_for_level(new_level) as f64) as i64;
    }

    /// Add points to the total experience
    pub const fn add_points(&mut self, additional_points: i64) {
        self.total_points += additional_points;
    }
}

#[cfg(test)]
mod tests {
    use crate::player::experience::Experience;

    #[test]
    fn test() {
        for i in 0..100 {
            let points = Experience::total_points_at_level(i);
            let level = Experience {
                total_points: points,
            }
            .level();
            assert_eq!(i, level, "level mismatch with points: {points}");
        }
    }
}
