/// Which way a picture is being scaled into the square that shows it,
/// which is the whole of how to draw it. Anything blown up past its own
/// pixels wants nearest-neighbour -- the faces here are pixel art, and
/// smoothing turns that to mush -- and anything being made smaller, a
/// photograph somebody dropped into the room, wants smoothing, which
/// nearest-neighbour would turn to aliasing.
///
/// Answered from the picture and not from where it came from: the same
/// 36-pixel square holds a 128-pixel sprite the AI drew and a 384-pixel
/// photograph somebody brought in, and only the picture knows which it
/// is. So it is asked once the browser has it, in an `onload`.
///
/// `box` is the size of that square in CSS pixels, given rather than
/// measured: a load event can arrive before the element has been laid
/// out, and a width of zero reads as "scaled down" when what it means
/// is "not on the screen yet".
export const scaledDown = (img, box) => img.naturalWidth >= box;
