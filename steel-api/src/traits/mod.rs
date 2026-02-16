use crate::abi_types::Uuid;
use abi_stable::sabi_trait;
use abi_stable::std_types::{RBox, RString};
use steel_utils::math::Vector3;
use steel_utils::types::GameType;

define_trait_with_arc_forward! {
    #[sabi_trait]
    pub trait Entity {
        fn pitch(&self) -> f32;
        fn yaw(&self) -> f32;

        fn set_rotation(&self, pitch: f32, yaw: f32);

        // fn look_at(&self);
        fn get_server(&self) -> Server_TO<'static, RBox<()>>;
        fn get_world(&self) -> World_TO<'static, RBox<()>>;
        // is_in_water, teleport
        // get_nearby_entities, get_entity_id, play_effect, get_type, get_pose, is_sneaking, set_sneaking, set_pose,

        fn velocity(&self) -> Vector3<f64>;
        fn set_velocity(&self, velocity: Vector3<f64>);

        fn on_ground(&self) -> bool;
        fn set_on_ground(&self, value: bool);

        fn uuid(&self) -> Uuid;
        // fn entity_type(&self) -> EntityTypeRef; steel-registry

        fn position(&self) -> Vector3<f64>;
        fn set_position(&self, pos: Vector3<f64>);

        fn get_eye_height(&self) -> f64;
        // fn get_bounding_box() -> AABBd; steel-registry

    }

    #[sabi_trait]
    pub trait Player {
        fn get_name(&self) -> RString;
        // fn kick(&self);
        // get_address, get_protocol_version
        // is_whitelisted, set_whitelisted
        // set_title_times, set_subtitle, show_title
        // play_sound, play_effect, send_action_bar, send_message
        fn set_allow_flight(&self, value: bool);
        fn get_allow_flight(&self) -> bool;
        fn is_flying(&self) -> bool;
        fn set_flying(&self, value: bool);

        // set_fly_speed, set_walk_speed, get_fly_speed, get_walk_speed
        // get_scoreboard, set_scoreboard, spawn_particle, get_client_brand_name, set_rotation, look_at, give
        // get_inventory, get_equipment, get_ender_chest, get_item_in_hand, open_inventory, close_inventory, set_item_in_hand
        fn get_game_mode(&self) -> GameType;
        fn set_game_mode(&self, value: GameType);

        // get_body_yaw, set_body_jaw
    }

    #[sabi_trait]
    pub trait Server {

    }

    #[sabi_trait]
    pub trait World {

    }
}
