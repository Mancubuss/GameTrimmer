//! Spike: can Windows re-encode a game's own WMV cutscene, without any
//! third-party tool?
//!
//! ffmpeg reads VC-1 and WMV9 and cannot write either. Windows itself ships
//! the encoder (`wmvencod.dll`, registered as "WMVideo9 Encoder DMO" and as a
//! Media Foundation transform), which is the only licence-free path to the
//! 39 GB of `.wmv` in a real library. This asks the cheapest possible version
//! of the question - the WinRT `MediaTranscoder`, a dozen calls instead of a
//! COM pipeline - and prints what actually came out.
//!
//! Run: `cargo run -p gametrimmer-core --example wmv_spike -- <in.wmv> <out.wmv> <width> <height>`

use windows::core::HSTRING;
use windows::Media::MediaProperties::MediaEncodingProfile;
use windows::Media::Transcoding::MediaTranscoder;
use windows::Storage::StorageFile;

fn main() -> windows::core::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: wmv_spike <in.wmv> <out.wmv> <width> <height>");
        std::process::exit(2);
    }
    let source_path = std::path::Path::new(&args[1]);
    let target_path = std::path::Path::new(&args[2]);
    let width: u32 = args[3].parse().expect("width");
    let height: u32 = args[4].parse().expect("height");

    let source = StorageFile::GetFileFromPathAsync(&HSTRING::from(source_path.as_os_str()))?.join()?;

    // StorageFolder sits behind another feature gate; an empty file made the
    // ordinary way and then opened is the same thing with less ceremony.
    let _ = std::fs::remove_file(target_path);
    std::fs::File::create(target_path).expect("create the target file");
    let target =
        StorageFile::GetFileFromPathAsync(&HSTRING::from(target_path.as_os_str()))?.join()?;

    // Read the source's own shape, then ask for the same profile with a
    // smaller frame. Whether this preserves the 5.1 WMA Pro track games ship,
    // or quietly folds it into stereo, is the whole point of the spike.
    let source_profile = MediaEncodingProfile::CreateFromFileAsync(&source)?.join()?;
    println!("source video: {:?}", describe_video(&source_profile));
    println!("source audio: {:?}", describe_audio(&source_profile));

    let profile = MediaEncodingProfile::CreateFromFileAsync(&source)?.join()?;
    let video = profile.Video()?;
    video.SetWidth(width)?;
    video.SetHeight(height)?;
    // 0.10 bits per pixel per frame, the figure measured on x264. WMV9 is a
    // weaker codec, so treat whatever comes out as data, not as the answer.
    let fps = video.FrameRate()?;
    let rate = if fps.Denominator()? > 0 {
        f64::from(fps.Numerator()?) / f64::from(fps.Denominator()?)
    } else {
        30.0
    };
    video.SetBitrate((f64::from(width) * f64::from(height) * rate * 0.10) as u32)?;
    println!("target video: {:?}", describe_video(&profile));
    println!("target audio: {:?}", describe_audio(&profile));

    let transcoder = MediaTranscoder::new()?;
    let prepared = transcoder
        .PrepareFileTranscodeAsync(&source, &target, &profile)?
        .join()?;
    if !prepared.CanTranscode()? {
        println!("REFUSED: {:?}", prepared.FailureReason()?);
        std::process::exit(1);
    }

    let started = std::time::Instant::now();
    prepared.TranscodeAsync()?.join()?;
    println!("transcoded in {:.1}s", started.elapsed().as_secs_f64());
    Ok(())
}

fn describe_video(profile: &MediaEncodingProfile) -> String {
    match profile.Video() {
        Ok(video) => format!(
            "{} {}x{} @ {} bit/s",
            video.Subtype().map(|s| s.to_string()).unwrap_or_default(),
            video.Width().unwrap_or(0),
            video.Height().unwrap_or(0),
            video.Bitrate().unwrap_or(0)
        ),
        Err(error) => format!("none ({error})"),
    }
}

fn describe_audio(profile: &MediaEncodingProfile) -> String {
    match profile.Audio() {
        Ok(audio) => format!(
            "{} {} ch @ {} Hz, {} bit/s",
            audio.Subtype().map(|s| s.to_string()).unwrap_or_default(),
            audio.ChannelCount().unwrap_or(0),
            audio.SampleRate().unwrap_or(0),
            audio.Bitrate().unwrap_or(0)
        ),
        Err(error) => format!("none ({error})"),
    }
}
