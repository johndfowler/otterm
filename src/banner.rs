//! Static ASCII art for the startup banner. Everything here is `const`
//! so rendering the splash screen never allocates for the art itself.

/// Big block-letter wordmark, rendered in river-teal on the splash screen.
pub const TITLE: &str = r"
 ██████╗ ████████╗████████╗███████╗██████╗ ███╗   ███╗
██╔═══██╗╚══██╔══╝╚══██╔══╝██╔════╝██╔══██╗████╗ ████║
██║   ██║   ██║      ██║   █████╗  ██████╔╝██╔████╔██║
██║   ██║   ██║      ██║   ██╔══╝  ██╔══██╗██║╚██╔╝██║
╚██████╔╝   ██║      ██║   ███████╗██║  ██║██║ ╚═╝ ██║
 ╚═════╝    ╚═╝      ╚═╝   ╚══════╝╚═╝  ╚═╝╚═╝     ╚═╝";

/// The mascot: small eyes, wide muzzle, whiskers. Definitely not an alien.
pub const OTTER: &str = r"
            .--.__.--.
           /          \
          |   o    o   |
       ---=     ..     =---
           \   '--'   /
          .-'`-.__.-'`-.
         /    .----.    \
        |    /  ()  \    |
      ~ ~\   \ .__. /   / ~ ~
       ~ ~'-..'----'..-'~ ~
";

/// One period of the water animation. The splash renders a window into two
/// copies of this laid end to end, offset by the tick counter, so the waves
/// scroll without allocating a new string each frame.
pub const WAVES: &str = "  ~    ~~     ~   ~~~    ~     ~~   ~    ~~~   ~  ";

pub const TAGLINE: &str = "the output librarian — never lose a scrollback again";
pub const HINT: &str = "press any key to dive in  ·  q to quit";
