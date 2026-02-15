use abi_stable::sabi_trait;
use abi_stable::std_types::RString;

define_trait_with_arc_forward! {
    #[sabi_trait]
    pub trait Entity {
        // get location, set_velocity, get_height, get_width, get_bounding_box, is_on_ground, is_in_water, get_world, set_rotation, teleport, look_at
        // get_nearby_entities, get_entity_id, get_server, get_uuid, play_effect, get_type, get_pose, is_sneaking, set_sneaking, set_pose,
        // get x/y/z/yaw/pitch

    }

    #[sabi_trait]
    pub trait Player {
        fn get_name(&self) -> RString;
        fn kick(&self);
        // get_location
        // get_address, get_protocol_version
        // get_uuid
        // is_whitelisted, set_whitelisted
        // set_title_times, set_subtitle, show_title
        // play_sound, play_effect, send_action_bar, send_message
        // get_allow_flight, set_allow_flight, is_flying, set_flying, set_fly_speed, set_walk_speed, get_fly_speed, get_walk_speed
        // get_scoreboard, set_scoreboard, spawn_particle, get_client_brand_name, set_rotation, look_at, give
        // get_inventory, get_equipment, get_ender_chest, get_item_in_hand, open_inventory, close_inventory, set_item_in_hand
        // get_game_mode, set_game_mode

        // get_body_yaw, set_body_jaw
    }


}

#[sabi_trait]
pub trait Server {

}