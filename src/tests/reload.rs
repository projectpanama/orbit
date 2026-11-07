//! Live config reload: a changed config reaches the running compositor,
//! and a broken file keeps what it said before.

use orbit_config::{Config, ConfigLoader, ConfigSource, Keysym, ModKey, Modifiers};

use super::{Fixture, map_window};
use crate::window::with_window_state;

fn super_held() -> Modifiers {
    Modifiers {
        meta: true,
        ..Modifiers::default()
    }
}

#[test]
fn reload_applies_binds_and_decorations() {
    let mut fixture = Fixture::new();
    fixture.add_output("A");
    let mut client = fixture.client();
    client.bind_globals();
    fixture.roundtrip(&mut client);
    let window = map_window(&mut fixture, &mut client);
    let _ = window;
    fixture.state.input.mod_key = ModKey::Super;

    let new = Config::parse(
        r##"
[binds]
"Mod+J" = { spawn = ["true"] }
[decorations]
height = 40
[decorations.colors]
active = "#ff0000"
"##,
    )
    .expect("valid");
    fixture.state.apply_config(new);

    let bound = fixture
        .state
        .config
        .bind_for(ModKey::Super, &super_held(), Keysym::j);
    assert!(bound.is_some());
    assert_eq!(fixture.state.decoration_style.config.height, 40);
    // Open windows take the new style too.
    let w = fixture
        .state
        .windows
        .values()
        .next()
        .cloned()
        .expect("window");
    let (height, red) =
        with_window_state(&w, |s| (s.style.height(), s.style.config.colors.active[0]));
    assert_eq!(height, 40);
    assert!((red - 1.0).abs() < 1e-6);
}

#[test]
fn a_broken_file_keeps_what_it_said_before() {
    let dir = std::env::temp_dir().join(format!("orbit-reload-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("config.toml");
    std::fs::write(&path, "[overview]\ndim = 0.5\n").expect("write");
    let mut loader = ConfigLoader::new(ConfigSource::Given(vec![path.clone()]));
    assert_eq!(loader.load().overview_dim, 0.5);
    std::fs::write(&path, "[overview\ndim = ").expect("write");
    assert_eq!(
        loader.load().overview_dim,
        0.5,
        "broken edit keeps the old value"
    );
    std::fs::write(&path, "[overview]\ndim = 0.2\n").expect("write");
    assert_eq!(loader.load().overview_dim, 0.2);
    std::fs::remove_file(&path).expect("remove");
    assert_ne!(
        loader.load().overview_dim,
        0.2,
        "a deleted file says nothing"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reloading_outputs_reapplies_scale() {
    let mut fixture = Fixture::with_outputs(&["A"]);
    let new = Config::parse("[outputs.A]\nscale = 2\n").expect("valid");
    fixture.state.apply_config(new);
    assert_eq!(fixture.output("A").current_scale().fractional_scale(), 2.0);
    // And back to automatic: the scale stays (nothing to pick it from).
    fixture.state.apply_config(Config::default());
    assert_eq!(fixture.output("A").current_scale().fractional_scale(), 2.0);
}

#[test]
fn layered_files_merge_and_a_broken_one_keeps_its_layer() {
    let dir = std::env::temp_dir().join(format!("orbit-layers-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let (base, over) = (dir.join("base.toml"), dir.join("over.toml"));
    std::fs::write(&base, "[overview]\ndim = 0.5\n[decorations]\nheight = 30\n").expect("write");
    std::fs::write(&over, "[decorations]\nheight = 40\n").expect("write");
    let mut loader = ConfigLoader::new(ConfigSource::Given(vec![base.clone(), over.clone()]));
    let config = loader.load();
    assert_eq!(config.overview_dim, 0.5, "from the base file");
    assert_eq!(config.decorations.height, 40, "the later file wins");

    // The later file breaks: its last good layer stays over the base.
    std::fs::write(&over, "[decorations\n").expect("write");
    std::fs::write(&base, "[overview]\ndim = 0.2\n[decorations]\nheight = 30\n").expect("write");
    let config = loader.load();
    assert_eq!(config.overview_dim, 0.2);
    assert_eq!(config.decorations.height, 40);

    // Only the base is left: it alone is the config.
    std::fs::remove_file(&over).expect("remove");
    let config = loader.load();
    assert_eq!(config.overview_dim, 0.2);
    assert_eq!(config.decorations.height, 30);
    let _ = std::fs::remove_dir_all(&dir);
}
