#version 450
layout(location = 0) in vec2 position;
layout(location = 1) in vec4 vertex_color;
#ifdef M5_FLAT
layout(location = 0) flat out vec4 color;
#else
layout(location = 0) out vec4 color;
#endif
void main()
{
    gl_Position = vec4(position, 0.0, 1.0);
    color = vertex_color;
#ifdef M5_PSIZ
    gl_PointSize = 1.0;
#endif
}
