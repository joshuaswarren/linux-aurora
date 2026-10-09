/* SPDX-License-Identifier: MIT */
/* Native GL 2.x / GLES 3.0 pixel smoke test. This is not a conformance suite. */
#define _GNU_SOURCE
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl3.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void fail(const char *message)
{
   fprintf(stderr, "FAIL: %s (EGL=%#x GL=%#x)\n", message, eglGetError(), glGetError());
   exit(1);
}

static GLuint shader(GLenum type, const char *source)
{
   GLuint object = glCreateShader(type);
   glShaderSource(object, 1, &source, NULL);
   glCompileShader(object);
   GLint ok = 0;
   glGetShaderiv(object, GL_COMPILE_STATUS, &ok);
   if (!ok) {
      char log[4096];
      glGetShaderInfoLog(object, sizeof(log), NULL, log);
      fprintf(stderr, "Shader: %s\n", log);
      fail("shader compilation");
   }
   return object;
}

static void provenance(void)
{
   const char *expected = getenv("ASAHI_GL_EXPECT_DRIVER");
   if (!expected || !*expected)
      fail("ASAHI_GL_EXPECT_DRIVER must identify the tested libgallium");
   FILE *maps = fopen("/proc/self/maps", "r");
   if (!maps)
      fail("cannot inspect loaded driver");
   char *line = NULL;
   size_t size = 0;
   int matched = 0;
   while (getline(&line, &size, maps) >= 0) {
      if (strstr(line, "libgallium")) {
         char *path = strchr(line, '/');
         if (path) {
            path[strcspn(path, "\n")] = 0;
            if (strcmp(path, expected))
               fail("unexpected Gallium library loaded");
            if (!matched)
               printf("DRIVER=%s\n", path);
            matched = 1;
         }
      }
      if (strstr(line, "libvulkan_asahi") || strstr(line, "libasahi_noop"))
         fail("Vulkan or no-op shim loaded during native GL test");
   }
   free(line);
   fclose(maps);
   if (!matched)
      fail("expected Gallium library was not loaded");
   const char *renderer = (const char *)glGetString(GL_RENDERER);
   if (!renderer || (!strstr(renderer, "Apple M5") && !strstr(renderer, "G17P")) ||
       strcasestr(renderer, "zink") || strcasestr(renderer, "llvmpipe") ||
       strcasestr(renderer, "softpipe"))
      fail("renderer is not native M5 Asahi");
   printf("RENDERER=%s\nVERSION=%s\nGLSL=%s\n", renderer,
          glGetString(GL_VERSION), glGetString(GL_SHADING_LANGUAGE_VERSION));
}

static void pixel(int x, int y, const unsigned char expected[4])
{
   unsigned char actual[4] = {0};
   glReadPixels(x, y, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, actual);
   if (glGetError() != GL_NO_ERROR)
      fail("pixel readback");
   for (unsigned c = 0; c < 4; ++c) {
      if (abs((int)actual[c] - expected[c]) > 2) {
         fprintf(stderr, "PIXEL (%d,%d) actual=%u,%u,%u,%u expected=%u,%u,%u,%u\n",
                 x, y, actual[0], actual[1], actual[2], actual[3],
                 expected[0], expected[1], expected[2], expected[3]);
         fail("pixel mismatch");
      }
   }
}

int main(int argc, char **argv)
{
   setbuf(stdout, NULL);
   if (argc != 2 || (strcmp(argv[1], "gl2") && strcmp(argv[1], "gles3"))) {
      fprintf(stderr, "Usage: %s gl2|gles3\n", argv[0]);
      return 2;
   }
   const int es = !strcmp(argv[1], "gles3");
   printf("REQUESTED_API=%s\n", es ? "OpenGL ES 3.0" : "OpenGL 2.0 compatible");
   EGLDisplay dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
   EGLint major = 0, minor = 0;
   if (!eglInitialize(dpy, &major, &minor))
      fail("eglInitialize");
   if (!eglBindAPI(es ? EGL_OPENGL_ES_API : EGL_OPENGL_API))
      fail("eglBindAPI");
   EGLint attrs[] = {EGL_SURFACE_TYPE, EGL_PBUFFER_BIT,
                    EGL_RENDERABLE_TYPE, es ? EGL_OPENGL_ES3_BIT : EGL_OPENGL_BIT,
                    EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8,
                    EGL_ALPHA_SIZE, 8, EGL_NONE};
   EGLConfig config;
   EGLint count;
   if (!eglChooseConfig(dpy, attrs, &config, 1, &count) || count != 1)
      fail("no requested API pbuffer config");
   EGLint context_attrs[] = {EGL_CONTEXT_MAJOR_VERSION, es ? 3 : 2,
                            EGL_CONTEXT_MINOR_VERSION, 0, EGL_NONE};
   EGLContext ctx = eglCreateContext(dpy, config, EGL_NO_CONTEXT, context_attrs);
   if (ctx == EGL_NO_CONTEXT)
      fail("eglCreateContext");
   EGLint surface_attrs[] = {EGL_WIDTH, 64, EGL_HEIGHT, 64, EGL_NONE};
   EGLSurface surface = eglCreatePbufferSurface(dpy, config, surface_attrs);
   if (surface == EGL_NO_SURFACE || !eglMakeCurrent(dpy, surface, surface, ctx))
      fail("pbuffer current");
   provenance();
   if (getenv("M5_GL_CONTEXT_ONLY")) {
      puts("NATIVE_CONTEXT_PASS (rendering untested)");
      eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
      eglDestroySurface(dpy, surface);
      eglDestroyContext(dpy, ctx);
      eglTerminate(dpy);
      return 0;
   }
   const char *vs = es ?
      "#version 300 es\nin vec2 position;void main(){gl_Position=vec4(position,0.,1.);}" :
      "#version 110\nattribute vec2 position;void main(){gl_Position=vec4(position,0.,1.);}";
   const char *fs = es ?
      "#version 300 es\nprecision highp float;uniform vec4 color;out vec4 result;void main(){result=color;}" :
      "#version 110\nuniform vec4 color;void main(){gl_FragColor=color;}";
   GLuint vertex = shader(GL_VERTEX_SHADER, vs), fragment = shader(GL_FRAGMENT_SHADER, fs);
   GLuint program = glCreateProgram();
   glAttachShader(program, vertex);
   glAttachShader(program, fragment);
   glBindAttribLocation(program, 0, "position");
   glLinkProgram(program);
   GLint linked = 0;
   glGetProgramiv(program, GL_LINK_STATUS, &linked);
   if (!linked) {
      char log[4096];
      glGetProgramInfoLog(program, sizeof(log), NULL, log);
      fprintf(stderr, "Program: %s\n", log);
      fail("program link");
   }
   glUseProgram(program);
   const GLfloat vertices[] = {-0.8f, -0.8f, 0.8f, -0.8f, 0.0f, 0.8f};
   GLuint buffer;
   glGenBuffers(1, &buffer);
   glBindBuffer(GL_ARRAY_BUFFER, buffer);
   glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW);
   glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, NULL);
   glEnableVertexAttribArray(0);
   glViewport(0, 0, 64, 64);
   const unsigned char blue[4] = {0, 0, 255, 255};
   for (unsigned frame = 0; frame < 3; ++frame) {
      const unsigned char expected[4] = {frame == 1 ? 0 : 255, frame == 1 ? 255 : 0, 0, 255};
      glClearColor(0, 0, 1, 1);
      glClear(GL_COLOR_BUFFER_BIT);
      glUniform4f(glGetUniformLocation(program, "color"), expected[0] / 255.f,
                  expected[1] / 255.f, 0, 1);
      glDrawArrays(GL_TRIANGLES, 0, 3);
      glFinish();
      if (glGetError() != GL_NO_ERROR)
         fail("draw");
      pixel(32, 24, expected);
      pixel(1, 62, blue);
      printf("NATIVE_FRAME_PASS=%u\n", frame);
   }
   glDeleteBuffers(1, &buffer);
   glDeleteProgram(program);
   glDeleteShader(vertex);
   glDeleteShader(fragment);
   eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
   eglDestroySurface(dpy, surface);
   eglDestroyContext(dpy, ctx);
   eglTerminate(dpy);
   printf("NATIVE_PIXEL_SMOKE_PASS=%s\n", argv[1]);
   return 0;
}
