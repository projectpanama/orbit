pub mod overview {
    #[cfg(feature = "server")]
    pub mod server {
        use wayland_protocols::ext::foreign_toplevel_list::v1::server::*;
        use wayland_server;
        use wayland_server::protocol::*;

        pub mod __interfaces {
            use wayland_backend;
            use wayland_protocols::ext::foreign_toplevel_list::v1::server::__interfaces::*;
            use wayland_server::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("../../protocols/orbit-overview-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("../../protocols/orbit-overview-v1.xml");
    }

    #[cfg(feature = "client")]
    pub mod client {
        use wayland_client;
        use wayland_client::protocol::*;
        use wayland_protocols::ext::foreign_toplevel_list::v1::client::*;

        pub mod __interfaces {
            use wayland_backend;
            use wayland_client::protocol::__interfaces::*;
            use wayland_protocols::ext::foreign_toplevel_list::v1::client::__interfaces::*;
            wayland_scanner::generate_interfaces!("../../protocols/orbit-overview-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("../../protocols/orbit-overview-v1.xml");
    }
}

pub mod toplevel_management {
    #[cfg(feature = "server")]
    pub mod server {
        use wayland_protocols::ext::{
            foreign_toplevel_list::v1::server::*, workspace::v1::server::*,
        };
        use wayland_server;
        use wayland_server::protocol::*;

        pub mod __interfaces {
            use wayland_backend;
            use wayland_protocols::ext::{
                foreign_toplevel_list::v1::server::__interfaces::*,
                workspace::v1::server::__interfaces::*,
            };
            use wayland_server::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!(
                "../../protocols/orbit-toplevel-management-v1.xml"
            );
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("../../protocols/orbit-toplevel-management-v1.xml");
    }

    #[cfg(feature = "client")]
    pub mod client {
        use wayland_client;
        use wayland_client::protocol::*;
        use wayland_protocols::ext::{
            foreign_toplevel_list::v1::client::*, workspace::v1::client::*,
        };

        pub mod __interfaces {
            use wayland_backend;
            use wayland_client::protocol::__interfaces::*;
            use wayland_protocols::ext::{
                foreign_toplevel_list::v1::client::__interfaces::*,
                workspace::v1::client::__interfaces::*,
            };
            wayland_scanner::generate_interfaces!(
                "../../protocols/orbit-toplevel-management-v1.xml"
            );
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("../../protocols/orbit-toplevel-management-v1.xml");
    }
}
