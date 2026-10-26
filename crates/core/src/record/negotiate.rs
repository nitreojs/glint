//! How frames reach the video encoder. The best [`Route`] is the hardware encoder of the GPU
//! that captures, fed the composed ARGB32 textures as they are (nothing is copied). Encoders that
//! only take NV12 (Intel Quick Sync, AMD AMF) get frames the same GPU converts first. When no
//! hardware encoder takes either, Windows' software encoder does, slower. Platform code tries
//! the routes in turn through [`negotiate`]; [`Failed::explain`] says in plain words why none
//! worked.

use super::Codec;
use super::pipeline::VideoConfig;

/// The frame format the encoder is fed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    /// The composed BGRA textures themselves.
    Argb,
    /// 4:2:0 YUV the GPU converts the composed image to (BT.709, limited range).
    Nv12,
}

impl Input {
    pub fn name(self) -> &'static str {
        match self {
            Input::Argb => "ARGB32",
            Input::Nv12 => "NV12",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    /// The capturing GPU's encoder.
    Hardware,
    /// Windows' encoder on the CPU.
    Software,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub engine: Engine,
    pub input: Input,
}

impl Route {
    /// How the frames get there, for the log: "NV12 via video processor".
    pub fn how(self) -> String {
        match (self.engine, self.input) {
            (_, Input::Nv12) => "NV12 via video processor".into(),
            (Engine::Hardware, Input::Argb) => "ARGB32 direct".into(),
            (Engine::Software, Input::Argb) => "ARGB32 converted on the CPU".into(),
        }
    }
}

/// How the YUV the encoder gets is made from RGB, and how players should read it back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Matrix {
    #[default]
    Bt709,
    Bt601,
}

impl Matrix {
    /// Its MatrixCoefficients code (ISO/IEC 23091-2), as `colr` boxes and H.264/HEVC VUI say it.
    pub fn code(self) -> u16 {
        match self {
            Matrix::Bt709 => 1,
            Matrix::Bt601 => 6,
        }
    }
}

/// The tallest picture Media Foundation reads as BT.601 when the stream doesn't say (measured:
/// the height alone decides).
const SD_LINES: u32 = 576;

/// The matrix frames are converted with on `route`, `height` lines tall: BT.709, which hardware
/// encoders write into the stream. Windows' software encoder writes no colour information, and
/// players then guess by the height, so its pictures up to [`SD_LINES`] are made BT.601 to be
/// read right (the file's `colr` box says so too, for players that look).
pub fn matrix(route: Route, height: u32) -> Matrix {
    match route.engine {
        Engine::Software if height <= SD_LINES => Matrix::Bt601,
        _ => Matrix::Bt709,
    }
}

/// Regions with a side below this are left to the software encoder without a word when the
/// GPU's encoder refuses them (NVIDIA's H.264 encoder wants 146×50 at least): it's quick at
/// that size, and the drivers aren't to blame.
const SMALL: u32 = 256;

/// What the user should hear, once, about recording `size` frames through `route` after the
/// routes `tried`: the software encoder is slower, and the GPU drivers may be why it's used.
pub fn notice(route: Route, tried: &[Tried], (w, h): (u32, u32)) -> Option<&'static str> {
    let refused = tried.iter().any(|t| t.route.engine == Engine::Hardware && matches!(t.why, Refusal::Format(_)));
    match route.engine {
        Engine::Software if refused && w.min(h) < SMALL => None,
        Engine::Software => Some("Recording with the software encoder: slower, check your GPU drivers."),
        Engine::Hardware => None,
    }
}

/// The encoder, adapter and route chosen, for the log: "Intel(R) Quick Sync Video H.264
/// Encoder MFT on Intel(R) UHD Graphics, NV12 via video processor".
pub fn describe(encoder: &str, adapter: &str, route: Route) -> String {
    match route.engine {
        Engine::Hardware => format!("{encoder} on {adapter}, {}", route.how()),
        Engine::Software => format!("{encoder} (software), {}", route.how()),
    }
}

/// Routes forced for testing: `GLINT_ENCODER=hardware|software`,
/// `GLINT_ENCODER_INPUT=argb|nv12`. Anything else is ignored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Force {
    pub engine: Option<Engine>,
    pub input: Option<Input>,
}

impl Force {
    pub fn from_env() -> Force {
        let var = |k| std::env::var(k).ok();
        Force::parse(var("GLINT_ENCODER").as_deref(), var("GLINT_ENCODER_INPUT").as_deref())
    }

    pub fn parse(engine: Option<&str>, input: Option<&str>) -> Force {
        let engine = match engine.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("hardware" | "hw") => Some(Engine::Hardware),
            Some("software" | "sw") => Some(Engine::Software),
            _ => None,
        };
        let input = match input.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("argb" | "argb32" | "bgra") => Some(Input::Argb),
            Some("nv12") => Some(Input::Nv12),
            _ => None,
        };
        Force { engine, input }
    }
}

/// The routes to try, best first. `convert`: the GPU can make NV12; without it the hardware
/// encoder only gets ARGB32 and the software one ARGB32 that Media Foundation converts.
pub fn routes(force: Force, convert: bool) -> Vec<Route> {
    let mut out = Vec::new();
    for engine in [Engine::Hardware, Engine::Software] {
        // the hardware encoder may take what we compose as it is; the software one takes NV12
        // and only gets ARGB32 through a converter on the CPU
        let inputs = match engine {
            Engine::Hardware => [Input::Argb, Input::Nv12],
            Engine::Software => [Input::Nv12, Input::Argb],
        };
        for input in inputs {
            let forced_out = force.engine.is_some_and(|e| e != engine) || force.input.is_some_and(|i| i != input);
            if !forced_out && (input == Input::Argb || convert) {
                out.push(Route { engine, input });
            }
        }
    }
    out
}

/// Why a route didn't work, in plain words (with the encoder's name and the HRESULT).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The encoder doesn't take this input (format, size or settings): the next may do.
    Format(String),
    /// No encoder for the codec on this engine: its other inputs aren't tried.
    Missing(String),
    /// Something no other route fixes (the file can't be created): nothing more is tried.
    Fatal(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tried {
    pub route: Route,
    pub why: Refusal,
}

/// Every route that was tried and why it didn't work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub tried: Vec<Tried>,
}

/// Tries `routes` in order until `attempt` takes one; returns what it made, the route and the
/// routes refused before it.
pub fn negotiate<T>(
    routes: &[Route],
    mut attempt: impl FnMut(Route) -> Result<T, Refusal>,
) -> Result<(T, Route, Vec<Tried>), Failed> {
    let mut tried: Vec<Tried> = Vec::new();
    for &route in routes {
        let missing = tried.iter().any(|t| t.route.engine == route.engine && matches!(t.why, Refusal::Missing(_)));
        if missing {
            continue;
        }
        match attempt(route) {
            Ok(made) => return Ok((made, route, tried)),
            Err(why) => {
                let fatal = matches!(why, Refusal::Fatal(_));
                tried.push(Tried { route, why });
                if fatal {
                    break;
                }
            }
        }
    }
    Err(Failed { tried })
}

/// Rates to offer the encoders for a recording at `fps`: as asked and, above 60, whole fractions
/// of it down to 30, which keep motion even (every second or third frame). Some encoders refuse
/// high rates (one NVIDIA laptop GPU took 60 fps but not 165 at 1440p; Windows' software
/// encoder stops near 172).
pub fn rates(fps: f64) -> Vec<f64> {
    let mut out = vec![fps];
    if fps > 60.0 {
        out.extend((2..).map(|k| fps / k as f64).take_while(|&r| r >= 30.0));
    }
    out
}

/// [`negotiate`] at the first of [`rates`] some route takes: what it made, the route, the rate
/// and the routes refused at it. A lower rate is only tried while every route refused the
/// frames themselves (a missing encoder or a file that can't be made won't change). The error
/// is the one at the rate asked for.
pub fn negotiate_rate<T>(
    fps: f64,
    routes: &[Route],
    mut attempt: impl FnMut(f64, Route) -> Result<T, Refusal>,
) -> Result<(T, Route, f64, Vec<Tried>), Failed> {
    let mut first: Option<Failed> = None;
    for rate in rates(fps) {
        match negotiate(routes, |route| attempt(rate, route)) {
            Ok((made, route, tried)) => return Ok((made, route, rate, tried)),
            Err(failed) => {
                let format = failed.tried.iter().any(|t| matches!(t.why, Refusal::Format(_)));
                let fatal = failed.tried.iter().any(|t| matches!(t.why, Refusal::Fatal(_)));
                let stop = !format || fatal;
                if !stop {
                    log_tried(&failed.tried);
                    crate::log::log(&format!("no encoder took {} fps, trying less", fps_text(rate)));
                }
                first.get_or_insert(failed);
                if stop {
                    break;
                }
            }
        }
    }
    Err(first.unwrap_or(Failed { tried: Vec::new() }))
}

/// Logs the routes to the encoder that didn't work.
pub fn log_tried(tried: &[Tried]) {
    for t in tried {
        let (Refusal::Format(why) | Refusal::Missing(why) | Refusal::Fatal(why)) = &t.why;
        crate::log::log(&format!("{:?} encoder, {} frames: {why}", t.route.engine, t.route.input.name()));
    }
}

/// A rate as people say it: `165`, `82.50`.
pub fn fps_text(fps: f64) -> String {
    if fps.fract().abs() < 0.01 { format!("{:.0}", fps) } else { format!("{:.2}", fps) }
}

impl Failed {
    /// The error the user sees: what was tried on `adapter` for `config`'s frames and why each
    /// didn't work. The size and rate are in it: encoders refuse some (odd sides, high rates).
    pub fn explain(&self, config: &VideoConfig, adapter: &str) -> String {
        let codec = config.codec;
        if let Some(Tried { why: Refusal::Fatal(e), .. }) = self.tried.last() {
            return e.clone();
        }
        if self.tried.is_empty() {
            return format!("no way to encode {} was allowed (check GLINT_ENCODER / GLINT_ENCODER_INPUT)", codec.name());
        }
        let missing = self.tried.iter().all(|t| matches!(t.why, Refusal::Missing(_)));
        let mut text = if missing {
            format!("{adapter} can't encode {}:", codec.name())
        } else {
            let fps = fps_text(config.fps);
            format!("no encoder took {}×{} {} at {fps} fps on {adapter}:", config.width, config.height, codec.name())
        };
        for t in &self.tried {
            let (Refusal::Format(why) | Refusal::Missing(why) | Refusal::Fatal(why)) = &t.why;
            match t.why {
                Refusal::Missing(_) => text += &format!("\n· {why}"),
                _ => text += &format!("\n· {} frames: {why}", t.route.input.name()),
            }
        }
        if missing && codec == Codec::Hevc {
            text += "\nChoose H.264 in settings.";
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HW_ARGB: Route = Route { engine: Engine::Hardware, input: Input::Argb };
    const HW_NV12: Route = Route { engine: Engine::Hardware, input: Input::Nv12 };
    const SW_NV12: Route = Route { engine: Engine::Software, input: Input::Nv12 };
    const SW_ARGB: Route = Route { engine: Engine::Software, input: Input::Argb };

    fn config(codec: Codec) -> VideoConfig {
        VideoConfig { codec, width: 2560, height: 1440, fps: 165.0, qp: 20, effort: 50, keyframe_secs: 1.0, bitrate: None }
    }

    /// An encoder that takes only `takes`, logging what it was offered.
    struct Fake {
        takes: Vec<Route>,
        missing: Vec<Engine>,
        offered: Vec<Route>,
    }

    impl Fake {
        fn try_route(&mut self, r: Route) -> Result<&'static str, Refusal> {
            self.offered.push(r);
            if self.missing.contains(&r.engine) {
                return Err(Refusal::Missing(format!("no {:?} encoder", r.engine)));
            }
            match self.takes.contains(&r) {
                true => Ok("encoder"),
                false => Err(Refusal::Format(format!("refused {} (0xC00D36B4)", r.input.name()))),
            }
        }
    }

    fn fake(takes: &[Route], missing: &[Engine]) -> Fake {
        Fake { takes: takes.to_vec(), missing: missing.to_vec(), offered: Vec::new() }
    }

    #[test]
    fn hardware_argb_comes_first_then_nv12_then_software() {
        assert_eq!(routes(Force::default(), true), [HW_ARGB, HW_NV12, SW_NV12, SW_ARGB]);
        assert_eq!(routes(Force::default(), false), [HW_ARGB, SW_ARGB], "no NV12 without the GPU converting");
    }

    #[test]
    fn forcing_keeps_only_the_matching_routes() {
        let nv12 = Force::parse(None, Some("NV12"));
        assert_eq!(routes(nv12, true), [HW_NV12, SW_NV12]);
        let sw = Force::parse(Some("software"), None);
        assert_eq!(routes(sw, true), [SW_NV12, SW_ARGB]);
        assert_eq!(routes(Force::parse(Some("hw"), Some("argb")), true), [HW_ARGB]);
        assert_eq!(Force::parse(Some("gpu"), Some("yuv")), Force::default(), "unknown values are ignored");
    }

    #[test]
    fn an_encoder_refusing_argb_gets_nv12_from_the_gpu() {
        let mut enc = fake(&[HW_NV12, SW_NV12], &[]);
        let (made, route, tried) = negotiate(&routes(Force::default(), true), |r| enc.try_route(r)).unwrap();
        assert_eq!((made, route), ("encoder", HW_NV12));
        assert_eq!(enc.offered, [HW_ARGB, HW_NV12]);
        assert_eq!(tried, [Tried { route: HW_ARGB, why: Refusal::Format("refused ARGB32 (0xC00D36B4)".into()) }]);
        assert_eq!(notice(route, &tried, (1920, 1080)), None);
        assert_eq!(describe("Intel(R) Quick Sync Video H.264 Encoder MFT", "Intel(R) UHD Graphics", route),
            "Intel(R) Quick Sync Video H.264 Encoder MFT on Intel(R) UHD Graphics, NV12 via video processor");
    }

    #[test]
    fn argb_is_kept_when_the_encoder_takes_it() {
        let mut enc = fake(&[HW_ARGB, HW_NV12], &[]);
        let (_, route, tried) = negotiate(&routes(Force::default(), true), |r| enc.try_route(r)).unwrap();
        assert_eq!((route, tried.len()), (HW_ARGB, 0));
    }

    #[test]
    fn without_a_hardware_encoder_the_software_one_records_and_says_so() {
        let mut enc = fake(&[SW_NV12], &[Engine::Hardware]);
        let (_, route, tried) = negotiate(&routes(Force::default(), true), |r| enc.try_route(r)).unwrap();
        assert_eq!(route, SW_NV12);
        assert_eq!(enc.offered, [HW_ARGB, SW_NV12], "a missing encoder isn't offered NV12 too");
        assert!(notice(route, &tried, (100, 100)).unwrap().contains("software encoder"));
    }

    #[test]
    fn only_small_software_pictures_are_bt601() {
        assert_eq!(matrix(HW_NV12, 360), Matrix::Bt709);
        assert_eq!(matrix(SW_NV12, 576), Matrix::Bt601);
        assert_eq!(matrix(SW_ARGB, 360), Matrix::Bt601);
        assert_eq!(matrix(SW_NV12, 578), Matrix::Bt709);
        assert_eq!((Matrix::Bt709.code(), Matrix::Bt601.code()), (1, 6));
    }

    #[test]
    fn a_region_too_small_for_the_gpu_goes_to_the_software_encoder_quietly() {
        let mut enc = fake(&[SW_NV12], &[]);
        let (_, route, tried) = negotiate(&routes(Force::default(), true), |r| enc.try_route(r)).unwrap();
        assert_eq!(route, SW_NV12);
        assert_eq!(notice(route, &tried, (120, 40)), None);
        assert!(notice(route, &tried, (1920, 1080)).is_some(), "a big one is news");
    }

    #[test]
    fn a_fatal_error_stops_the_search() {
        let mut offered = 0;
        let r = negotiate(&routes(Force::default(), true), |_| -> Result<(), _> {
            offered += 1;
            Err(Refusal::Fatal("MFCreateSinkWriterFromURL: access denied".into()))
        });
        assert_eq!(offered, 1);
        assert_eq!(r.unwrap_err().explain(&config(Codec::H264), "GPU"), "MFCreateSinkWriterFromURL: access denied");
    }

    #[test]
    fn the_error_names_the_adapter_the_encoders_and_why() {
        let mut enc = fake(&[], &[]);
        let failed = negotiate(&routes(Force::default(), true), |r| enc.try_route(r)).unwrap_err();
        assert_eq!(failed.tried.len(), 4);
        assert_eq!(
            failed.explain(&config(Codec::H264), "Intel(R) UHD Graphics"),
            "no encoder took 2560×1440 H.264 at 165 fps on Intel(R) UHD Graphics:\n\
             · ARGB32 frames: refused ARGB32 (0xC00D36B4)\n\
             · NV12 frames: refused NV12 (0xC00D36B4)\n\
             · NV12 frames: refused NV12 (0xC00D36B4)\n\
             · ARGB32 frames: refused ARGB32 (0xC00D36B4)"
        );
    }

    #[test]
    fn no_hevc_encoder_anywhere_says_so_plainly() {
        let mut enc = fake(&[], &[Engine::Hardware, Engine::Software]);
        let failed = negotiate(&routes(Force::default(), true), |r| enc.try_route(r)).unwrap_err();
        assert_eq!(enc.offered, [HW_ARGB, SW_NV12]);
        assert_eq!(
            failed.explain(&config(Codec::Hevc), "Intel(R) UHD Graphics"),
            "Intel(R) UHD Graphics can't encode HEVC:\n· no Hardware encoder\n· no Software encoder\nChoose H.264 in settings."
        );
    }

    #[test]
    fn high_rates_fall_back_to_whole_fractions_down_to_30() {
        assert_eq!(rates(165.0), [165.0, 82.5, 55.0, 41.25, 33.0]);
        assert_eq!(rates(240.0), [240.0, 120.0, 80.0, 60.0, 48.0, 40.0, 240.0 / 7.0, 30.0]);
        assert_eq!(rates(60.0), [60.0], "60 and below is recorded as asked or not at all");
    }

    #[test]
    fn a_rate_every_route_refuses_is_halved_until_one_takes_it() {
        let routes = routes(Force::default(), true);
        let mut offered = Vec::new();
        let (_, route, rate, tried) = negotiate_rate(165.0, &routes, |rate, r| {
            offered.push(rate);
            // the hardware encoder takes up to 144 fps, software nothing at all
            if r.engine == Engine::Hardware && rate <= 144.0 { Ok(()) } else { Err(Refusal::Format(format!("refused {rate}"))) }
        })
        .unwrap();
        assert_eq!((route, rate), (HW_ARGB, 82.5));
        assert!(tried.is_empty(), "only refusals at the rate that worked are reported with it");
        assert_eq!(offered, [165.0, 165.0, 165.0, 165.0, 82.5]);
    }

    #[test]
    fn missing_encoders_and_fatal_errors_dont_try_lower_rates() {
        let routes = routes(Force::default(), true);
        let mut asked = Vec::new();
        let failed = negotiate_rate(240.0, &routes, |rate, _| -> Result<(), _> {
            asked.push(rate);
            Err(Refusal::Missing("no encoder".into()))
        })
        .unwrap_err();
        assert!(asked.iter().all(|&r| r == 240.0), "{asked:?}");
        assert!(failed.tried.iter().all(|t| matches!(t.why, Refusal::Missing(_))));
        let mut asked = Vec::new();
        let failed = negotiate_rate(240.0, &routes, |rate, _| -> Result<(), _> {
            asked.push(rate);
            Err(Refusal::Fatal("can't make the file".into()))
        })
        .unwrap_err();
        assert_eq!(asked, [240.0]);
        assert_eq!(failed.explain(&config(Codec::H264), "GPU"), "can't make the file");
    }

    #[test]
    fn when_no_rate_works_the_error_is_about_the_rate_asked_for() {
        let failed = negotiate_rate(165.0, &routes(Force::default(), true), |rate, _| -> Result<(), _> {
            Err(Refusal::Format(format!("refused {rate}")))
        })
        .unwrap_err();
        assert!(failed.tried.iter().all(|t| t.why == Refusal::Format("refused 165".into())), "{failed:?}");
    }
}
