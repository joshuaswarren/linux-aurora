/* Native EGL device test for GPU context lifetime; no X server or VirtualGL. */
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define CHECK(x) do { if (!(x)) { fprintf(stderr, "FAIL line %d: %s\n", __LINE__, #x); return 1; } } while (0)

static GLuint shader(GLenum type, const char *text)
{
    GLuint s = glCreateShader(type);
    GLint ok = 0;
    glShaderSource(s, 1, &text, NULL);
    glCompileShader(s);
    glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[1024];
        glGetShaderInfoLog(s, sizeof(log), NULL, log);
        fprintf(stderr, "Shader: %s\n", log);
        glDeleteShader(s);
        return 0;
    }
    return s;
}

int main(int argc, char **argv)
{
    CHECK(argc == 2);
    PFNEGLQUERYDEVICESEXTPROC query = (void *)eglGetProcAddress("eglQueryDevicesEXT");
    PFNEGLQUERYDEVICESTRINGEXTPROC device_string = (void *)eglGetProcAddress("eglQueryDeviceStringEXT");
    PFNEGLGETPLATFORMDISPLAYEXTPROC platform = (void *)eglGetProcAddress("eglGetPlatformDisplayEXT");
    CHECK(query && device_string && platform);
    EGLDeviceEXT devices[16], selected = EGL_NO_DEVICE_EXT;
    EGLint count = 0;
    CHECK(query(16, devices, &count));
    for (int i = 0; i < count; ++i) {
        const char *node = device_string(devices[i], EGL_DRM_RENDER_NODE_FILE_EXT);
        if (node && !strcmp(node, argv[1])) selected = devices[i];
    }
    CHECK(selected != EGL_NO_DEVICE_EXT);
    EGLDisplay d = platform(EGL_PLATFORM_DEVICE_EXT, selected, NULL);
    CHECK(d != EGL_NO_DISPLAY && eglInitialize(d, NULL, NULL));
    CHECK(eglBindAPI(EGL_OPENGL_ES_API));
    const EGLint attrs[] = { EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8,
        EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8, EGL_NONE };
    EGLConfig config;
    CHECK(eglChooseConfig(d, attrs, &config, 1, &count) && count == 1);
    const EGLint context_attrs[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
    EGLContext c = eglCreateContext(d, config, EGL_NO_CONTEXT, context_attrs);
    CHECK(c != EGL_NO_CONTEXT);
    const EGLint surface_attrs[] = { EGL_WIDTH, 64, EGL_HEIGHT, 64, EGL_NONE };
    EGLSurface surface = eglCreatePbufferSurface(d, config, surface_attrs);
    CHECK(surface != EGL_NO_SURFACE && eglMakeCurrent(d, surface, surface, c));
    const char *renderer = (const char *)glGetString(GL_RENDERER);
    CHECK(renderer && !strstr(renderer, "llvmpipe") && !strstr(renderer, "softpipe"));
    CHECK(strstr(renderer, "Apple") || strstr(renderer, "AGX") || strstr(renderer, "Asahi"));
    printf("RENDERER %s\n", renderer);
    fflush(stdout);

    GLuint vs = shader(GL_VERTEX_SHADER, "attribute vec2 p; void main(){gl_Position=vec4(p,0.,1.);}");
    GLuint fs = shader(GL_FRAGMENT_SHADER, "precision highp float; uniform vec4 color; void main(){gl_FragColor=color;}");
    CHECK(vs && fs);
    GLuint program = glCreateProgram();
    glAttachShader(program, vs); glAttachShader(program, fs);
    glBindAttribLocation(program, 0, "p"); glLinkProgram(program);
    GLint linked = 0; glGetProgramiv(program, GL_LINK_STATUS, &linked); CHECK(linked);
    glUseProgram(program);
    GLint color = glGetUniformLocation(program, "color"); CHECK(color >= 0);
    const GLfloat vertices[] = { -1, -1, 3, -1, -1, 3 };
    GLuint vbo; glGenBuffers(1, &vbo); glBindBuffer(GL_ARRAY_BUFFER, vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW);
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, NULL);
    glEnableVertexAttribArray(0); glViewport(0, 0, 64, 64); glDisable(GL_DITHER);
    for (unsigned frame = 0; frame < 60; ++frame) {
        unsigned char expected[4] = { (frame * 17 + 19) % 256, (frame * 31 + 43) % 256, (frame * 7 + 91) % 256, 255 };
        unsigned char pixel[4] = { 0 };
        glUniform4f(color, expected[0]/255.f, expected[1]/255.f, expected[2]/255.f, 1.f);
        glDrawArrays(GL_TRIANGLES, 0, 3);
        glReadPixels(32, 32, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, pixel);
        CHECK(glGetError() == GL_NO_ERROR);
        for (unsigned channel = 0; channel < 4; ++channel)
            CHECK(abs((int)pixel[channel] - expected[channel]) <= 2);
    }
    glDeleteBuffers(1, &vbo); glDeleteProgram(program); glDeleteShader(vs); glDeleteShader(fs);
    CHECK(eglMakeCurrent(d, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT));
    CHECK(eglDestroySurface(d, surface) && eglDestroyContext(d, c) && eglTerminate(d));
    puts("NATIVE_CONTEXT_PIXELS_PASS frames=60");
    return 0;
}
