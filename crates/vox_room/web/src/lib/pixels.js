/// Which way a picture is being scaled into the square that shows it,
/// which is the whole of how to draw it. Anything blown up past its own
/// pixels wants nearest-neighbour -- a sprite is drawn one colour to a
/// pixel and smoothing turns that to mush -- and anything being made
/// smaller wants smoothing, which nearest-neighbour would turn to
/// aliasing.
///
/// Answered from the picture and not from where it came from, because
/// where it came from does not settle it. The same 36-pixel square
/// holds a sprite somebody dropped in at its own 64 pixels, a
/// cel-shaded portrait the AI drew at 768, and a sprite the AI drew
/// that its server handed back already magnified so it would be
/// legible in a chat -- the first blown up here and the other two
/// shrunk, though two of the three are sprites. Only the picture
/// knows which it is, so it is asked once the browser has it, in an
/// `onload`.
///
/// `box` is the size of that square in CSS pixels, given rather than
/// measured: a load event can arrive before the element has been laid
/// out, and a width of zero reads as "scaled down" when what it means
/// is "not on the screen yet".
export const scaledDown = (img, box) => img.naturalWidth >= box;
