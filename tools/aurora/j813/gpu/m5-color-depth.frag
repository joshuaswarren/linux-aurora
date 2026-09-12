#version 450
#ifdef M5_FLAT
layout(location = 0) flat in vec4 color;
#else
layout(location = 0) in vec4 color;
#endif
layout(location = 0) out vec4 out_color;
void main()
{
    out_color = color;
}
