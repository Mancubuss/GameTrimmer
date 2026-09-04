//! Shrinking a game's own cutscenes by re-encoding them at a lower
//! resolution - the engine half of the video slimming feature.
//!
//! # What this deliberately does not do
//!
//! Three quarters of the video mass in a real library (920 GB of 1 182 GB
//! measured on the owner's own 1 637 games) sits in Bink 1, Bink 2, CriWare
//! USM/CPK, VP6 and WMV/VC-1. No encoder for any of them exists - not in this
//! project, not in any build of `ffmpeg`, because the formats are closed. A
//! file in one of those is left alone, and [`plan`] says so by name rather
//! than failing later.
//!
//! For the quarter that is left, the operation is deliberately narrow:
//!
//! - **The codec never changes.** The game picks its decoder, not the user:
//!   Unity's VideoPlayer reads VP8, an Unreal 3 title with Theora reads
//!   Theora. Handing either an HEVC file is the same breakage as handing it
//!   a text file, only less obvious. It also costs nothing to stay put -
//!   re-encoding a 4K 40 Mbit/s H.264 cutscene to 720p H.264 measured 96.3%
//!   smaller, against 96.3% for HEVC.
//! - **Audio is copied, never re-encoded.** It is a few percent of the bytes,
//!   and touching it is how a 5.1 stream gets downmixed into an engine that
//!   assumed six channels, or a seamless loop loses its sample-accurate
//!   length.
//! - **Every stream is kept** (`-map 0`). Left to itself `ffmpeg` maps one
//!   video and one audio stream, silently dropping the other language tracks
//!   of a multi-track cutscene.
//! - **Frames are counted before and after, and a mismatch voids the
//!   result.** This is not belt-and-braces: a `libtheora` encode that died at
//!   frame 2 872 of 20 087 left behind a valid, playable, 97%-smaller file.
//!   The exit code is not enough, the file existing is not enough, and the
//!   file being smaller is exactly the symptom.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{CoreError, Result};

/// Containers whose files can be rewritten in place. An extension outside
/// this list is left alone even when `ffprobe` reads it happily: `.usm`
/// decodes fine and cannot be written back.
const REWRITABLE_EXTENSIONS: &[&str] = &["mp4", "m4v", "mov", "webm", "mkv", "ogv", "avi"];

/// A re-encode that would give back less than this share of the file is not
/// worth a lossy, irreversible rewrite of someone's game.
const MIN_SHRINK_RATIO: f64 = 0.30;

/// Locations of the external tools. The feature does not exist without them.
#[derive(Debug, Clone)]
pub struct Tools {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

/// What `ffprobe` says about a file's first video stream.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoInfo {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration_secs: f64,
    pub size_bytes: u64,
}

/// Why a file is being left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaveReason {
    /// The container cannot be written back - Bink, USM, CPK and friends.
    ClosedContainer,
    /// The container is fine but nothing here can encode that codec back into
    /// it (VP6 in an `.avi`, VC-1 in a `.mkv`).
    NoEncoder(String),
    /// Re-encoding would save less than [`MIN_SHRINK_RATIO`] of the file.
    AlreadySmallEnough,
}

/// A transcode that is worth doing, with the exact command it will run.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub source: PathBuf,
    pub staged: PathBuf,
    pub target_width: u32,
    pub target_height: u32,
    pub estimated_bytes: u64,
    args: Vec<String>,
}

impl Plan {
    /// The saving this plan is expected to produce, in bytes.
    pub fn estimated_saving(&self, current_bytes: u64) -> u64 {
        current_bytes.saturating_sub(self.estimated_bytes)
    }
}

/// The outcome of [`Plan`], accepted only after the frame count matched.
#[derive(Debug, Clone, PartialEq)]
pub struct Shrunk {
    pub was_bytes: u64,
    pub now_bytes: u64,
    pub frames: u64,
}

impl Tools {
    /// Finds `ffmpeg` and `ffprobe`, preferring a folder the user pointed at.
    /// `None` means the feature stays invisible - there is nothing sensible
    /// to do without them and nothing to report either.
    pub fn discover(configured_dir: Option<&Path>) -> Option<Tools> {
        let candidates = configured_dir
            .map(|dir| (dir.join(exe("ffmpeg")), dir.join(exe("ffprobe"))))
            .into_iter()
            .chain(std::iter::once((
                PathBuf::from(exe("ffmpeg")),
                PathBuf::from(exe("ffprobe")),
            )));

        for (ffmpeg, ffprobe) in candidates {
            if runs(&ffmpeg) && runs(&ffprobe) {
                return Some(Tools { ffmpeg, ffprobe });
            }
        }
        None
    }

    /// Reads the first video stream's shape. Errors are the caller's cue to
    /// leave the file alone, not to retry.
    pub fn probe(&self, path: &Path) -> Result<VideoInfo> {
        let out = self.run(
            &self.ffprobe,
            &[
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name,width,height,r_frame_rate",
                "-show_entries",
                "format=duration",
                "-of",
                "default=nw=1:nk=1",
                "--",
                &path.to_string_lossy(),
            ],
        )?;

        let fields: Vec<&str> = out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        if fields.len() < 5 {
            return Err(CoreError::Other(format!(
                "ffprobe told us too little about {}: {out:?}",
                path.display()
            )));
        }

        Ok(VideoInfo {
            codec: fields[0].to_string(),
            width: fields[1].parse().unwrap_or(0),
            height: fields[2].parse().unwrap_or(0),
            fps: parse_rational(fields[3]),
            duration_secs: fields[4].parse().unwrap_or(0.0),
            size_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        })
    }

    /// Counts video packets - one per frame for every codec here. Exact, and
    /// it costs a full read of the file, which is why it is only ever spent
    /// on a file that is about to be, or has just been, re-encoded.
    pub fn count_frames(&self, path: &Path) -> Result<u64> {
        let out = self.run(
            &self.ffprobe,
            &[
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-count_packets",
                "-show_entries",
                "stream=nb_read_packets",
                "-of",
                "csv=p=0",
                "--",
                &path.to_string_lossy(),
            ],
        )?;
        out.trim().parse().map_err(|_| {
            CoreError::Other(format!("no frame count for {}: {out:?}", path.display()))
        })
    }

    /// Runs the plan and returns the result only if it is provably the same
    /// video: same frame count, same codec, and smaller. Anything else leaves
    /// the original untouched and the staged file removed.
    pub fn shrink(&self, plan: &Plan, source: &VideoInfo) -> Result<Shrunk> {
        let before = self.count_frames(&plan.source)?;

        let args: Vec<&str> = plan.args.iter().map(String::as_str).collect();
        let staged = &plan.staged;
        let outcome = self.run(&self.ffmpeg, &args);
        let verified = outcome.and_then(|_| self.verify(staged, source, before));

        match verified {
            Ok(shrunk) => {
                crate::atomic_file::replace(staged, &plan.source)?;
                Ok(shrunk)
            }
            Err(error) => {
                let _ = std::fs::remove_file(staged);
                Err(error)
            }
        }
    }

    fn verify(&self, staged: &Path, source: &VideoInfo, before: u64) -> Result<Shrunk> {
        let produced = self.probe(staged)?;
        let after = self.count_frames(staged)?;

        if after != before {
            return Err(CoreError::Other(format!(
                "re-encode produced {after} frames instead of {before} - discarded"
            )));
        }
        if produced.codec != source.codec {
            return Err(CoreError::Other(format!(
                "re-encode produced {} instead of {} - discarded",
                produced.codec, source.codec
            )));
        }
        if produced.size_bytes >= source.size_bytes {
            return Err(CoreError::Other(format!(
                "re-encode produced {} bytes against the original's {} - discarded",
                produced.size_bytes, source.size_bytes
            )));
        }

        Ok(Shrunk {
            was_bytes: source.size_bytes,
            now_bytes: produced.size_bytes,
            frames: after,
        })
    }

    fn run(&self, tool: &Path, args: &[&str]) -> Result<String> {
        let output = command(tool).args(args).output()?;
        if !output.status.success() {
            return Err(CoreError::Other(format!(
                "{} failed: {}",
                tool.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// The screen the cutscene is being shrunk for - the feature's only knob.
/// Everything the card's six-row device matrix said reduces to these two
/// numbers once the codec is fixed by the engine rather than by us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    pub width: u32,
    pub height: u32,
}

impl Cap {
    /// Steam Deck, ROG Ally in saver mode, Legion Go at integer 2x.
    pub const HANDHELD: Cap = Cap {
        width: 1280,
        height: 800,
    };
    /// A desktop monitor, where 800p would be visible and 1080p is not.
    pub const DESKTOP: Cap = Cap {
        width: 1920,
        height: 1080,
    };
}

/// Decides what to do with one file. Pure: it looks at the probe result and
/// the path, runs nothing, and is the whole of the policy.
pub fn plan(path: &Path, info: &VideoInfo, cap: Cap) -> std::result::Result<Plan, LeaveReason> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !REWRITABLE_EXTENSIONS.contains(&extension.as_str()) {
        return Err(LeaveReason::ClosedContainer);
    }

    let encoder =
        encoder_for(&info.codec).ok_or_else(|| LeaveReason::NoEncoder(info.codec.clone()))?;

    let (target_width, target_height) = fit(info.width, info.height, cap);
    let fps = if info.fps > 0.0 { info.fps } else { 30.0 };
    let estimated_bytes = (f64::from(target_width)
        * f64::from(target_height)
        * fps
        * encoder.bits_per_pixel
        * info.duration_secs
        / 8.0) as u64;

    if info.size_bytes == 0
        || estimated_bytes >= info.size_bytes
        || (info.size_bytes - estimated_bytes) as f64 / (info.size_bytes as f64) < MIN_SHRINK_RATIO
    {
        return Err(LeaveReason::AlreadySmallEnough);
    }

    let staged = staged_path(path, &extension);
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-v".into(),
        "error".into(),
        "-y".into(),
        "-i".into(),
        path.to_string_lossy().into_owned(),
        "-map".into(),
        "0".into(),
        "-vf".into(),
        format!("scale={target_width}:{target_height}"),
        "-fps_mode".into(),
        "passthrough".into(),
        "-c:a".into(),
        "copy".into(),
        "-c:s".into(),
        "copy".into(),
        "-c:v".into(),
        encoder.name.into(),
    ];
    args.extend(encoder.quality.iter().map(|a| (*a).to_string()));
    args.push(staged.to_string_lossy().into_owned());

    Ok(Plan {
        source: path.to_path_buf(),
        staged,
        target_width,
        target_height,
        estimated_bytes,
        args,
    })
}

struct Encoder {
    name: &'static str,
    quality: &'static [&'static str],
    /// Bits per pixel per frame this encoder needs at the quality below,
    /// measured on real library files rather than guessed.
    bits_per_pixel: f64,
}

fn encoder_for(codec: &str) -> Option<Encoder> {
    Some(match codec {
        "h264" => Encoder {
            name: "libx264",
            quality: &["-crf", "23", "-preset", "medium", "-pix_fmt", "yuv420p"],
            bits_per_pixel: 0.105,
        },
        "hevc" => Encoder {
            name: "libx265",
            quality: &["-crf", "26", "-preset", "medium", "-pix_fmt", "yuv420p"],
            bits_per_pixel: 0.075,
        },
        "vp8" => Encoder {
            name: "libvpx",
            quality: &["-crf", "30", "-b:v", "2M"],
            bits_per_pixel: 0.130,
        },
        "vp9" => Encoder {
            name: "libvpx-vp9",
            quality: &["-crf", "32", "-b:v", "0"],
            bits_per_pixel: 0.090,
        },
        // `-g 64`, and the power of two is the point. libtheora packs its
        // granule position by shifting the keyframe number by the width of
        // the keyframe interval; handed ffmpeg's default it dies mid-file
        // with `theora_encode_packetout failed [-1]` and leaves a truncated
        // file behind. A power of two pushes the failure much further out but
        // does not always remove it, which is why the frame check in
        // `verify` is load-bearing for this codec in particular.
        "theora" => Encoder {
            name: "libtheora",
            quality: &["-q:v", "6", "-g", "64"],
            bits_per_pixel: 0.140,
        },
        _ => return None,
    })
}

/// Fits `width`x`height` inside the cap, keeping the aspect ratio and landing
/// on even dimensions - `yuv420p` has no odd sizes, and stretching a 16:9
/// cutscene into a 16:10 screen is the one thing a player will notice.
fn fit(width: u32, height: u32, cap: Cap) -> (u32, u32) {
    if width == 0 || height == 0 || (width <= cap.width && height <= cap.height) {
        return (even(width), even(height));
    }
    let scale = f64::min(
        f64::from(cap.width) / f64::from(width),
        f64::from(cap.height) / f64::from(height),
    );
    (
        even((f64::from(width) * scale).round() as u32),
        even((f64::from(height) * scale).round() as u32),
    )
}

fn even(value: u32) -> u32 {
    value.max(2) & !1
}

fn staged_path(path: &Path, extension: &str) -> PathBuf {
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    path.with_file_name(format!("{stem}.gt-shrink.{extension}"))
}

fn parse_rational(value: &str) -> f64 {
    match value.split_once('/') {
        Some((num, den)) => {
            let den: f64 = den.parse().unwrap_or(0.0);
            if den == 0.0 {
                0.0
            } else {
                num.parse::<f64>().unwrap_or(0.0) / den
            }
        }
        None => value.parse().unwrap_or(0.0),
    }
}

fn exe(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn runs(tool: &Path) -> bool {
    command(tool)
        .arg("-version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn command(tool: &Path) -> Command {
    let mut command = Command::new(tool);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: this runs from a GUI app, and a console flashing
        // up once per probed file is not a progress indicator.
        command.creation_flags(0x0800_0000);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(codec: &str, width: u32, height: u32, secs: f64, size: u64) -> VideoInfo {
        VideoInfo {
            codec: codec.to_string(),
            width,
            height,
            fps: 30.0,
            duration_secs: secs,
            size_bytes: size,
        }
    }

    /// The whole point of the feature's scope. ffmpeg reads Bink 1 happily -
    /// it just cannot write it back, and a file it cannot write back must
    /// never reach a plan.
    #[test]
    fn a_bink_file_is_left_alone_however_readable_it_is() {
        let left = plan(
            Path::new("movies/intro.bik"),
            &info("binkvideo", 3840, 2160, 380.0, 2_013_369_804),
            Cap::HANDHELD,
        );
        assert_eq!(left, Err(LeaveReason::ClosedContainer));
    }

    #[test]
    fn a_criware_usm_is_left_alone_even_though_it_holds_plain_mpeg1() {
        let left = plan(
            Path::new("Videos/01-defendcores.usm"),
            &info("mpeg1video", 960, 544, 120.0, 45_304_192),
            Cap::HANDHELD,
        );
        assert_eq!(left, Err(LeaveReason::ClosedContainer));
    }

    #[test]
    fn an_open_container_holding_a_codec_we_cannot_write_names_the_codec() {
        let left = plan(
            Path::new("movies/cutscene.avi"),
            &info("vp6", 640, 480, 90.0, 120_000_000),
            Cap::HANDHELD,
        );
        assert_eq!(left, Err(LeaveReason::NoEncoder("vp6".to_string())));
    }

    /// A file already encoded sensibly is not worth a lossy rewrite, and this
    /// is the guard that keeps the feature from touching most of a library.
    #[test]
    fn a_file_already_near_its_target_is_left_alone() {
        let left = plan(
            Path::new("movies/logo.mp4"),
            &info("h264", 1280, 720, 10.0, 3_500_000),
            Cap::HANDHELD,
        );
        assert_eq!(left, Err(LeaveReason::AlreadySmallEnough));
    }

    /// The measured case: Immortals of Aveum's 4K 40 Mbit/s cutscene, which
    /// really did come out 96% smaller at 1280x720.
    #[test]
    fn a_four_k_cutscene_is_planned_down_to_the_screen_and_keeps_its_codec() {
        let plan = plan(
            Path::new("Movies/DigicMovieWithPrePost.mp4"),
            &info("h264", 3840, 2160, 188.2, 1_842_716_664),
            Cap::HANDHELD,
        )
        .expect("a 40 Mbit/s 4K file is worth shrinking");

        assert_eq!((plan.target_width, plan.target_height), (1280, 720));
        assert!(plan.args.iter().any(|a| a == "libx264"));
        assert!(
            plan.args
                .windows(2)
                .any(|w| w[0] == "-c:a" && w[1] == "copy"),
            "audio must be copied, never re-encoded: {:?}",
            plan.args
        );
        assert!(
            plan.args.windows(2).any(|w| w[0] == "-map" && w[1] == "0"),
            "every stream must be kept: {:?}",
            plan.args
        );
        assert!(plan.estimated_saving(1_842_716_664) > 1_500_000_000);
    }

    /// libtheora dies mid-file on ffmpeg's default keyframe interval. The
    /// power of two is not a style choice.
    #[test]
    fn theora_is_always_given_a_power_of_two_keyframe_interval() {
        let plan = plan(
            Path::new("video/BR_BTS.ogv"),
            &info("theora", 1920, 1080, 671.0, 1_291_449_120),
            Cap::HANDHELD,
        )
        .expect("a 1080p Theora cutscene at 15 Mbit/s is worth shrinking");

        let g = plan
            .args
            .windows(2)
            .find(|w| w[0] == "-g")
            .map(|w| w[1].parse::<u32>().expect("-g takes a number"))
            .expect("theora must carry an explicit keyframe interval");
        assert!(g.is_power_of_two(), "-g {g} is not a power of two");
    }

    #[test]
    fn fitting_preserves_the_aspect_ratio_and_lands_on_even_sides() {
        assert_eq!(fit(1920, 1080, Cap::HANDHELD), (1280, 720));
        assert_eq!(fit(3840, 2160, Cap::HANDHELD), (1280, 720));
        assert_eq!(fit(3840, 1600, Cap::HANDHELD), (1280, 532));
        assert_eq!(fit(1024, 576, Cap::HANDHELD), (1024, 576));
        assert_eq!(fit(3840, 2160, Cap::DESKTOP), (1920, 1080));
    }

    /// A lossless 2-second clip, big enough to be worth shrinking.
    fn test_clip(dir: &Path) -> PathBuf {
        let source = dir.join("cutscene.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=1280x720:rate=30:duration=2",
                "-c:v",
                "libx264",
                "-qp",
                "0",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&source)
            .status()
            .expect("run ffmpeg");
        assert!(made.success(), "could not build the test clip");
        source
    }

    const SMALL: Cap = Cap {
        width: 320,
        height: 240,
    };

    /// End to end against the real tools, skipped where they are absent -
    /// which is also the state the feature ships in until someone installs
    /// them.
    #[test]
    fn a_real_file_shrinks_and_keeps_every_frame() {
        let Some(tools) = Tools::discover(None) else {
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let source = test_clip(dir.path());

        let before = tools.probe(&source).expect("probe the test clip");
        let plan = plan(&source, &before, SMALL).expect("a lossless clip is worth shrinking");
        let shrunk = tools.shrink(&plan, &before).expect("shrink the test clip");

        assert_eq!(shrunk.frames, 60, "two seconds at 30 fps");
        assert!(shrunk.now_bytes < shrunk.was_bytes / 2);
        assert!(
            !plan.staged.exists(),
            "the staged file must not be left behind"
        );

        let after = tools.probe(&source).expect("probe the replaced file");
        assert_eq!(after.codec, "h264", "the codec must not have changed");
        assert_eq!((after.width, after.height), (320, 180));
    }

    /// The counter-example the frame check exists for. A half-finished encode
    /// is exactly what a `libtheora` failure leaves behind on a real library
    /// file: smaller, playable, and missing two thirds of the cutscene. If
    /// this test ever passes silently, the guard above proves nothing.
    #[test]
    fn a_truncated_encode_is_discarded_and_the_original_survives() {
        let Some(tools) = Tools::discover(None) else {
            return;
        };
        let dir = tempfile::tempdir().expect("temp dir");
        let source = test_clip(dir.path());

        let before = tools.probe(&source).expect("probe the test clip");
        let mut plan = plan(&source, &before, SMALL).expect("a lossless clip is worth shrinking");
        // Stop the encoder half way, the way a dying codec does.
        let output = plan.args.len() - 1;
        plan.args.insert(output, "30".into());
        plan.args.insert(output, "-frames:v".into());

        let error = tools
            .shrink(&plan, &before)
            .expect_err("half a cutscene must never replace a whole one");
        assert!(
            error.to_string().contains("30 frames instead of 60"),
            "the refusal must name the mismatch: {error}"
        );

        assert!(
            !plan.staged.exists(),
            "the half-encoded file must not be left behind"
        );
        assert_eq!(
            tools.probe(&source).expect("probe the original"),
            before,
            "the original must be untouched"
        );
        assert_eq!(tools.count_frames(&source).expect("count the original"), 60);
    }
}
