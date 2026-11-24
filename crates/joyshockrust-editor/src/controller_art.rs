use adw::{
    glib,
    gtk::{cairo, gdk, gdk_pixbuf},
    prelude::*,
};

const SVG: &[u8] = include_bytes!("../assets/dualsense.svg");
pub const SIZE: f64 = 256.0;
const RASTER_SIZE: i32 = 768;

pub fn load() -> Result<gdk_pixbuf::Pixbuf, glib::Error> {
    let loader = gdk_pixbuf::PixbufLoader::with_type("svg")?;
    loader.set_size(RASTER_SIZE, RASTER_SIZE);
    loader.write(SVG)?;
    loader.close()?;
    loader.pixbuf().ok_or_else(|| {
        glib::Error::new(
            gdk_pixbuf::PixbufError::Failed,
            "Could not render DualSense art",
        )
    })
}

pub fn draw(cr: &cairo::Context, svg: &gdk_pixbuf::Pixbuf, color: gdk::RGBA, opacity: f64) {
    let _ = cr.save();
    cr.scale(
        SIZE / f64::from(svg.width()),
        SIZE / f64::from(svg.height()),
    );
    cr.set_source_pixbuf(svg, 0.0, 0.0);
    let mask = cr.source();
    // The supplied artwork uses currentColor. Use its alpha as a mask
    // so the outline follows the surrounding GTK theme.
    cr.set_source_rgba(
        color.red().into(),
        color.green().into(),
        color.blue().into(),
        opacity,
    );
    let _ = cr.mask(&mask);
    let _ = cr.restore();
}
