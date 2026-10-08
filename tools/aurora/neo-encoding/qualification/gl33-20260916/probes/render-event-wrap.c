/* Hardware completion-wrap probe. Every read checks freshly drawn pixels.
 * No glFinish, retry-read, or additional memory barrier hides a stale result. */
#define GL_GLEXT_PROTOTYPES
#include <EGL/egl.h>
#include <GL/gl.h>
#include <GL/glext.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static GLuint shader(GLenum type, const char *source)
{
    GLuint s = glCreateShader(type);
    glShaderSource(s, 1, &source, NULL);
    glCompileShader(s);
    GLint ok = 0;
    glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[2048]; glGetShaderInfoLog(s, sizeof(log), NULL, log);
        fprintf(stderr, "shader failed: %s\n", log); exit(2);
    }
    return s;
}

int main(void)
{
    const int width = 113, height = 47, iterations = 768;
    EGLDisplay display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    EGLint major, minor, n;
    EGLConfig config;
    const EGLint ca[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT, EGL_NONE};
    const EGLint xa[] = {EGL_CONTEXT_MAJOR_VERSION, 3,
        EGL_CONTEXT_MINOR_VERSION, 3, EGL_CONTEXT_OPENGL_PROFILE_MASK,
        EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT, EGL_NONE};
    const EGLint sa[] = {EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE};
    if (!eglInitialize(display, &major, &minor) || !eglBindAPI(EGL_OPENGL_API)
        || !eglChooseConfig(display, ca, &config, 1, &n) || n != 1) return 2;
    EGLContext context = eglCreateContext(display, config, EGL_NO_CONTEXT, xa);
    EGLSurface surface = eglCreatePbufferSurface(display, config, sa);
    if (context == EGL_NO_CONTEXT || surface == EGL_NO_SURFACE
        || !eglMakeCurrent(display, surface, surface, context)) return 2;
    const char *renderer = (const char *)glGetString(GL_RENDERER);
    printf("renderer=%s\n", renderer ? renderer : "NULL"); fflush(stdout);
    if (!renderer || !strstr(renderer, "Apple") || strstr(renderer, "llvmpipe")) return 2;
    GLuint program = glCreateProgram();
    GLuint vs = shader(GL_VERTEX_SHADER,
        "#version 330 core\n"
        "void main(){vec2 p=vec2((gl_VertexID<<1)&2,gl_VertexID&2);"
        "gl_Position=vec4(p*2.0-1.0,0,1);}");
    GLuint fs = shader(GL_FRAGMENT_SHADER,
        "#version 330 core\nuniform vec4 color;out vec4 result;"
        "void main(){result=color;}");
    glAttachShader(program, vs); glAttachShader(program, fs); glLinkProgram(program);
    GLint ok = 0; glGetProgramiv(program, GL_LINK_STATUS, &ok); if (!ok) return 2;
    glUseProgram(program);
    GLint color = glGetUniformLocation(program, "color"); if (color < 0) return 2;
    GLuint vao, texture, fbo;
    glGenVertexArrays(1, &vao); glBindVertexArray(vao);
    glGenTextures(1, &texture); glBindTexture(GL_TEXTURE_2D, texture);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, width, height, 0, GL_RGBA,
                 GL_UNSIGNED_BYTE, NULL);
    glGenFramebuffers(1, &fbo); glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
                           GL_TEXTURE_2D, texture, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) return 2;
    glViewport(0, 0, width, height); glDisable(GL_DITHER);
    unsigned char *pixels = malloc(width * height * 4); if (!pixels) return 2;
    for (int i = 0; i < iterations; ++i) {
        unsigned char expected[4] = {(i * 17 + 3) & 255,
            (i * 31 + 7) & 255, (i * 67 + 11) & 255, 255};
        glUniform4f(color, expected[0] / 255.0f, expected[1] / 255.0f,
                    expected[2] / 255.0f, 1.0f);
        memset(pixels, 0xe5, width * height * 4);
        glDrawArrays(GL_TRIANGLES, 0, 3);
        glReadPixels(0, 0, width, height, GL_RGBA, GL_UNSIGNED_BYTE, pixels);
        GLenum error = glGetError();
        if (error) { fprintf(stderr, "FAIL iteration=%d GLerror=%#x\n", i, error); return 1; }
        for (int p = 0; p < width * height; ++p) {
            if (memcmp(pixels + p * 4, expected, 4)) {
                fprintf(stderr, "FAIL iteration=%d pixel=%d got=%u,%u,%u,%u expected=%u,%u,%u,%u\n",
                    i, p, pixels[p*4], pixels[p*4+1], pixels[p*4+2], pixels[p*4+3],
                    expected[0], expected[1], expected[2], expected[3]); return 1;
            }
        }
    }
    printf("PIXELS_PASS draws=%d pixels=%d first_read_only=1\n", iterations,
           iterations * width * height);
    free(pixels); eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroySurface(display, surface); eglDestroyContext(display, context);
    eglTerminate(display);
    return 0;
}
