//! Pictures: bytes on disk to pixels in a buffer.
//!
//! Two formats decode here, PNG and JPEG, and they were not chosen by taste.
//! PNG is what a screenshot is and what the colour emoji font stores its glyphs
//! as, so it earns its place twice. JPEG is what a photograph is. Everything
//! else -- GIF, WebP, AVIF, TIFF -- is a decoder's worth of code and attack
//! surface for a file type that does not turn up in a markdown document about
//! software.
//!
//! SVG is the pointed omission. It is the format a diagram is in, and it is
//! refused for the same reason `resvg` was refused when mermaid needed a
//! renderer: rasterising SVG means a font database, and scanning fonts costs
//! more than this entire program's startup budget. Mermaid fences are drawn as
//! text instead, which is the case that actually mattered.
//!
//! # Why a decoder at all, rather than the `image` crate
//!
//! `image` is the obvious dependency and it is a facade over exactly these
//! decoders plus a dozen more, a colour-management layer, and a resampling
//! library. What is wanted here is two decoders and one box filter. The filter
//! is thirty lines.

/// A decoded picture: 0xAARRGGBB per pixel, alpha NOT premultiplied.
///
/// Straight alpha rather than premultiplied because the renderer blends onto an
/// opaque ground with the same `blend` it uses for glyph coverage, and that
/// wants a colour and a coverage value separately. Premultiplying would mean two
/// blend paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

/// The most pixels that will be decoded from one file.
///
/// A compressed image declares its own dimensions, so a 40-kilobyte file may
/// claim to be 60,000 by 60,000 and cost fourteen gigabytes to expand. That is a
/// known trick and the defence is to refuse before allocating, not after.
///
/// 16 megapixels is twice a 4K screen and far past anything a document puts on
/// a page. It is deliberately not larger, because the ceiling is a real
/// allocation and not a notional one: the decoder wants four bytes a pixel and
/// this crate keeps four more, so 16 megapixels already costs 128MB at the
/// limit. Set at 64 it was half a gigabyte for one picture, in a program whose
/// whole argument is that it is small.
const MAX_PIXELS: usize = 16 << 20;

impl Bitmap {
    /// A picture scaled to `nw` by `nh`, by averaging the source pixels that
    /// fall under each destination pixel.
    ///
    /// A box filter, not nearest-neighbour sampling. The difference is not
    /// subtle here: colour emoji are stored at 136 pixels and drawn at about 22,
    /// so nearest-neighbour throws away thirty-five of every thirty-six pixels
    /// and what arrives on screen is noise shaped vaguely like a face.
    ///
    /// Averaged in PREMULTIPLIED space. Averaging straight alpha mixes the
    /// colour of transparent pixels into opaque ones, which puts a halo of
    /// whatever the transparent pixels happened to contain -- usually black --
    /// around every edge.
    pub fn resized(&self, nw: usize, nh: usize) -> Bitmap {
        let (nw, nh) = (nw.max(1), nh.max(1));
        if nw == self.w && nh == self.h {
            return self.clone();
        }
        let mut px = vec![0u32; nw * nh];
        for y in 0..nh {
            // The half-open source band this destination row covers.
            let sy0 = y * self.h / nh;
            let sy1 = (((y + 1) * self.h).div_ceil(nh)).max(sy0 + 1).min(self.h);
            for x in 0..nw {
                let sx0 = x * self.w / nw;
                let sx1 = (((x + 1) * self.w).div_ceil(nw)).max(sx0 + 1).min(self.w);
                // 64-bit accumulators. A destination pixel can cover a very
                // large number of source pixels when the reduction is extreme --
                // a picture in a window dragged to its narrowest -- and 255
                // times that overflows 32 bits. This binary is built with
                // `panic = "abort"`, so in release an opaque white picture came
                // back at alpha 0x35 and in debug the process died.
                let (mut a, mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
                for sy in sy0..sy1 {
                    for sx in sx0..sx1 {
                        let p = u64::from(self.px[sy * self.w + sx]);
                        let pa = (p >> 24) & 0xff;
                        a += pa;
                        r += (((p >> 16) & 0xff) * pa) / 255;
                        g += (((p >> 8) & 0xff) * pa) / 255;
                        b += ((p & 0xff) * pa) / 255;
                        n += 1;
                    }
                }
                if n == 0 {
                    continue;
                }
                let (a, r, g, b) = (a / n, r / n, g / n, b / n);
                // Back out of premultiplied space, so the renderer gets the
                // colour the pixel would be if it were opaque.
                let un = |c: u64| (c * 255).checked_div(a).unwrap_or(0).min(255) as u32;
                px[y * nw + x] = ((a as u32) << 24) | (un(r) << 16) | (un(g) << 8) | un(b);
            }
        }
        Bitmap { w: nw, h: nh, px }
    }

    /// The size this picture should be drawn at to fit inside `max_w` by
    /// `max_h`, keeping its proportions and never enlarging it.
    ///
    /// Never enlarging is the part worth stating: a 32-pixel icon stretched to
    /// the width of the column is a blurred rectangle, and the author who wrote
    /// a 32-pixel icon meant a 32-pixel icon.
    pub fn fit(&self, max_w: f32, max_h: f32) -> (f32, f32) {
        let (w, h) = (self.w as f32, self.h as f32);
        if w <= 0.0 || h <= 0.0 {
            return (0.0, 0.0);
        }
        let scale = (max_w / w).min(max_h / h).min(1.0);
        (w * scale, h * scale)
    }
}

/// Decode a picture, or `None` if these bytes are not one this program reads.
///
/// The format is taken from the bytes rather than the file's extension. An
/// extension is a claim made by whoever named the file; the magic number is a
/// claim made by whatever wrote it, which is the one worth believing.
pub fn decode(bytes: &[u8]) -> Option<Bitmap> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        png(bytes)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        jpeg(bytes)
    } else {
        None
    }
}

fn png(bytes: &[u8]) -> Option<Bitmap> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    // EXPAND turns a palette into real colours and a tRNS chunk into an alpha
    // channel; the normalise turns 16-bit channels into 8. Without them this
    // would need four more arms below for formats that are the same picture.
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().ok()?;
    let info = reader.info();
    let (w, h) = (info.width as usize, info.height as usize);
    if w == 0 || h == 0 || w.checked_mul(h)? > MAX_PIXELS {
        return None;
    }
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buf).ok()?;
    let (w, h) = (frame.width as usize, frame.height as usize);
    let raw = &buf[..frame.buffer_size()];
    let px = match frame.color_type {
        png::ColorType::Rgba => raw.chunks_exact(4).map(|c| pack(c[3], c[0], c[1], c[2])).collect(),
        png::ColorType::Rgb => raw.chunks_exact(3).map(|c| pack(255, c[0], c[1], c[2])).collect(),
        png::ColorType::GrayscaleAlpha => {
            raw.chunks_exact(2).map(|c| pack(c[1], c[0], c[0], c[0])).collect()
        }
        png::ColorType::Grayscale => raw.iter().map(|g| pack(255, *g, *g, *g)).collect(),
        // Indexed survives EXPAND, so reaching here means the transformations
        // did not apply and the bytes cannot be interpreted as colours.
        png::ColorType::Indexed => return None,
    };
    Some(Bitmap { w, h, px })
}

fn jpeg(bytes: &[u8]) -> Option<Bitmap> {
    let mut dec = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(bytes));
    dec.decode_headers().ok()?;
    let (w, h) = dec.dimensions()?;
    if w == 0 || h == 0 || w.checked_mul(h)? > MAX_PIXELS {
        return None;
    }
    let raw = dec.decode().ok()?;
    // JPEG has no alpha, and the decoder is asked for nothing but RGB.
    let px: Vec<u32> = match raw.len() / (w * h) {
        3 => raw.chunks_exact(3).map(|c| pack(255, c[0], c[1], c[2])).collect(),
        1 => raw.iter().map(|g| pack(255, *g, *g, *g)).collect(),
        _ => return None,
    };
    Some(Bitmap { w, h, px })
}

#[inline]
fn pack(a: u8, r: u8, g: u8, b: u8) -> u32 {
    (u32::from(a) << 24) | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny PNG built at test time, so the test data is readable rather than a
    /// base64 blob nobody can check.
    fn png_bytes(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("header");
            writer.write_image_data(rgba).expect("data");
        }
        out
    }

    fn solid(w: usize, h: usize, colour: u32) -> Bitmap {
        Bitmap { w, h, px: vec![colour; w * h] }
    }

    #[test]
    fn a_png_decodes_to_the_colours_it_holds() {
        let bytes = png_bytes(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]);
        let b = decode(&bytes).expect("decodes");
        assert_eq!((b.w, b.h), (2, 1));
        assert_eq!(b.px[0], 0xff_ff0000, "first pixel is red");
        assert_eq!(b.px[1], 0xff_0000ff, "second pixel is blue");
    }

    #[test]
    fn transparency_survives_decoding() {
        // If alpha were dropped, every transparent pixel would arrive black and
        // opaque -- which is what a picture with a black box around it means.
        let bytes = png_bytes(1, 1, &[10, 20, 30, 0]);
        let b = decode(&bytes).expect("decodes");
        assert_eq!(b.px[0] >> 24, 0, "alpha was lost");
    }

    #[test]
    fn bytes_that_are_not_a_picture_are_refused_rather_than_guessed_at() {
        assert!(decode(b"# this is a markdown file\n").is_none());
        assert!(decode(&[]).is_none());
        // A truncated PNG: the magic is right and the rest is not.
        assert!(decode(&[0x89, b'P', b'N', b'G', 1, 2, 3]).is_none());
    }

    #[test]
    fn the_format_is_read_from_the_bytes_not_the_extension() {
        // The same bytes decode whatever they might have been called.
        let bytes = png_bytes(1, 1, &[1, 2, 3, 255]);
        assert!(decode(&bytes).is_some());
    }

    #[test]
    fn scaling_down_averages_rather_than_picks() {
        // Four pixels, two black and two white, into one. Nearest-neighbour
        // gives black or white; averaging gives grey. At the ratio emoji are
        // drawn at -- 136 pixels down to 22 -- picking throws away 35 of every
        // 36 pixels.
        let b = Bitmap {
            w: 2,
            h: 2,
            px: vec![0xff_000000, 0xff_ffffff, 0xff_ffffff, 0xff_000000],
        };
        let small = b.resized(1, 1);
        let grey = small.px[0] & 0xff;
        assert!((100..=155).contains(&grey), "not an average: {grey:#x}");
    }

    #[test]
    fn scaling_does_not_bleed_transparent_pixels_into_opaque_ones() {
        // Half opaque red, half fully transparent. Averaged in straight alpha
        // the transparent half's colour -- black -- mixes in and the result is
        // a dark red halo. Premultiplied, the colour stays red and only the
        // alpha falls.
        let b = Bitmap { w: 2, h: 1, px: vec![0xff_ff0000, 0x00_000000] };
        let one = b.resized(1, 1);
        let p = one.px[0];
        assert_eq!((p >> 24) & 0xff, 127, "alpha should be the average");
        assert!((p >> 16) & 0xff > 240, "red was diluted by a transparent pixel: {p:#x}");
    }

    #[test]
    fn scaling_to_the_same_size_changes_nothing() {
        let b = solid(3, 3, 0xff_123456);
        assert_eq!(b.resized(3, 3), b);
    }

    #[test]
    fn fitting_never_enlarges() {
        // A 32-pixel icon stretched across a 700-pixel column is a blurred
        // rectangle, and it is not what the author wrote.
        let b = solid(32, 32, 0);
        assert_eq!(b.fit(700.0, 700.0), (32.0, 32.0));
    }

    #[test]
    fn fitting_keeps_the_proportions() {
        let b = solid(1000, 500, 0);
        let (w, h) = b.fit(600.0, 10_000.0);
        assert!((w - 600.0).abs() < 0.01);
        assert!((h - 300.0).abs() < 0.01, "aspect ratio lost: {w}x{h}");
    }

    #[test]
    fn fitting_is_bounded_by_whichever_side_runs_out_first() {
        // A tall thin picture is limited by the height, not the width.
        let b = solid(100, 1000, 0);
        let (w, h) = b.fit(1000.0, 200.0);
        assert!((h - 200.0).abs() < 0.01, "height not honoured: {w}x{h}");
        assert!(w < 21.0);
    }

    #[test]
    fn an_extreme_reduction_does_not_wrap_the_accumulator() {
        // 17.6 megapixels is the smallest source that reproduces it: the alpha
        // accumulator sums 255 per pixel, and 16.8 million of those is where a
        // 32-bit total wraps. In release, where this binary aborts rather than
        // panics on overflow, an opaque white picture came back at alpha 0x35 --
        // silently eighty percent transparent.
        //
        // Reachable through the public API, and through the program itself when
        // a window is dragged narrow enough that a picture's box rounds to a
        // pixel or two.
        let n = 4200usize;
        let b = Bitmap { w: n, h: n, px: vec![0xff_ffffff; n * n] };
        let one = b.resized(1, 1);
        assert_eq!(one.px[0], 0xff_ffffff, "an opaque white picture came back as {:#010x}", one.px[0]);
    }

    #[test]
    fn a_picture_claiming_an_impossible_size_is_refused_before_it_is_allocated() {
        // The declared dimensions live in the header, so this is decidable
        // without expanding anything -- which is the whole point, since
        // expanding is what the attack costs.
        let mut header = png_bytes(1, 1, &[0, 0, 0, 255]);
        // IHDR width lives at byte 16, big-endian.
        header[16..20].copy_from_slice(&60_000u32.to_be_bytes());
        header[20..24].copy_from_slice(&60_000u32.to_be_bytes());
        assert!(decode(&header).is_none());
    }
}
