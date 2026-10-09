// SPDX-License-Identifier: GPL-2.0-only
/* Retain one GLES context, shader and VBO across an externally controlled sleep.
 * Usage: screen-sleep PHASE_FILE OUTPUT_DIRECTORY
 * PHASE_FILE: 0=before, 1=entering, 2=woken, 3=quit, 4=failed.
 * Escape also closes the window. This program never requests system sleep.
 */
#include <SDL2/SDL.h>
#include <GLES2/gl2.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <time.h>

static int width, height;
static const unsigned char letters[26][7] = {
	{14,17,17,31,17,17,17}, {30,17,17,30,17,17,30},
	{14,17,16,16,16,17,14}, {30,17,17,17,17,17,30},
	{31,16,16,30,16,16,31}, {31,16,16,30,16,16,16},
	{14,17,16,23,17,17,15}, {17,17,17,31,17,17,17},
	{14,4,4,4,4,4,14}, {7,2,2,2,18,18,12},
	{17,18,20,24,20,18,17}, {16,16,16,16,16,16,31},
	{17,27,21,21,17,17,17}, {17,25,21,19,17,17,17},
	{14,17,17,17,17,17,14}, {30,17,17,30,16,16,16},
	{14,17,17,17,21,18,13}, {30,17,17,30,20,18,17},
	{15,16,16,14,1,1,30}, {31,4,4,4,4,4,4},
	{17,17,17,17,17,17,14}, {17,17,17,17,17,10,4},
	{17,17,17,21,21,21,10}, {17,17,10,4,10,17,17},
	{17,17,10,4,4,4,4}, {31,1,2,4,8,16,31},
};
static const char *names[] = {"before", "entering", "after", "quit", "failed"};

static void fail(const char *what)
{
	fprintf(stderr, "%s: %s GL=%x\n", what, SDL_GetError(), glGetError());
	exit(1);
}

static GLuint shader(GLenum type, const char *source)
{
	GLuint s = glCreateShader(type);
	GLint ok;
	char log[1024];

	glShaderSource(s, 1, &source, NULL);
	glCompileShader(s);
	glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
	if (!ok) {
		glGetShaderInfoLog(s, sizeof(log), NULL, log);
		fprintf(stderr, "%s\n", log);
		fail("shader compile");
	}
	return s;
}

static void text(const char *s, int y, int scale)
{
	int x = (1000 - (int)strlen(s) * 6 * scale) / 2;

	glEnable(GL_SCISSOR_TEST);
	glClearColor(1, 1, 1, 1);
	for (; *s; s++, x += 6 * scale) {
		const unsigned char exclamation[7] = {4,4,4,4,4,0,4};
		const unsigned char colon[7] = {0,4,4,0,4,4,0};
		const unsigned char *glyph;
		if (*s >= 'A' && *s <= 'Z')
			glyph = letters[*s - 'A'];
		else if (*s == '!')
			glyph = exclamation;
		else if (*s == ':')
			glyph = colon;
		else
			continue;
		for (int row = 0; row < 7; row++)
			for (int col = 0; col < 5; col++) {
				if (!(glyph[row] & (1 << (4 - col))))
					continue;
				int left = (x + col * scale) * width / 1000;
				int right = (x + (col + 1) * scale) * width / 1000;
				int top = (y + row * scale) * height / 600;
				int bottom = (y + (row + 1) * scale) * height / 600;
				glScissor(left, height - bottom, right - left, bottom - top);
				glClear(GL_COLOR_BUFFER_BIT);
			}
	}
	glDisable(GL_SCISSOR_TEST);
}

static void draw(SDL_Window *window, GLuint program, int phase, const char *directory)
{
	unsigned char pixel[4], *pixels;
	char path[4096];
	FILE *f;
	const float color[5][3] = {{0.1f, 0.8f, 1.0f}, {1.0f, .65f, .1f},
		{0.95f, 0.3f, 0.75f}, {0,0,0}, {1,0,0}};
	const float background[5][3] = {{.035f,.08f,.20f}, {.20f,.10f,.015f},
		{.015f,.18f,.10f}, {0,0,0}, {.20f,.015f,.015f}};
	const char *title[] = {"BEFORE SLEEP", "ENTERING SLEEP", "WOKEN UP! :D", "", "TEST STOPPED"};
	const char *subtitle[] = {"WAITING FOR YOUR APPROVAL", "AUTOMATIC WAKE IN SIXTY SECONDS",
		"NEW FRAME DRAWN AFTER RESUME", "", "CHECK THE TEST LOG"};
	struct timespec stamp;

	SDL_GL_GetDrawableSize(window, &width, &height);
	glViewport(0, 0, width, height);
	glClearColor(background[phase][0], background[phase][1], background[phase][2], 1);
	glClear(GL_COLOR_BUFFER_BIT);
	glUseProgram(program);
	glUniform3fv(glGetUniformLocation(program, "color"), 1, color[phase]);
	glDrawArrays(GL_TRIANGLES, 0, 3);
	text(title[phase], 55, 8);
	text(subtitle[phase], 145, 3);
	text("SAME PROCESS AND GPU CONTEXT", 495, 4);
	text("ESC TO CLOSE", 560, 2);
	glFinish();
	glReadPixels(width / 2, height / 2, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, pixel);
	if (glGetError() != GL_NO_ERROR)
		fail("render/readback");
	printf("readback phase=%s size=%dx%d center=%u,%u,%u,%u expected=%u,%u,%u\n",
	       names[phase], width, height, pixel[0], pixel[1], pixel[2], pixel[3],
	       (unsigned)(color[phase][0] * 255 + .5f),
	       (unsigned)(color[phase][1] * 255 + .5f),
	       (unsigned)(color[phase][2] * 255 + .5f));
	pixels = malloc((size_t)width * height * 4);
	if (!pixels)
		fail("allocate capture");
	glReadPixels(0, 0, width, height, GL_RGBA, GL_UNSIGNED_BYTE, pixels);
	if (glGetError() != GL_NO_ERROR)
		fail("frame capture");
	unsigned char *center = pixels + ((size_t)(height / 2) * width + width / 2) * 4;
	printf("full-frame center=%u,%u,%u,%u\n", center[0], center[1], center[2], center[3]);
	snprintf(path, sizeof(path), "%s/%s.ppm", directory, names[phase]);
	f = fopen(path, "wb");
	if (!f)
		fail("open capture");
	fprintf(f, "P6\n%d %d\n255\n", width, height);
	for (int y = height - 1; y >= 0; y--)
		for (int x = 0; x < width; x++)
			if (fwrite(pixels + ((size_t)y * width + x) * 4, 3, 1, f) != 1)
				fail("write capture");
	if (fclose(f))
		fail("close capture");
	free(pixels);
	SDL_GL_SwapWindow(window);
	glFinish();
	clock_gettime(CLOCK_MONOTONIC, &stamp);
	printf("phase=%s pid=%d context=%p program=%u size=%dx%d pixel=%u,%u,%u GL=%x\n",
	       names[phase], getpid(), SDL_GL_GetCurrentContext(), program,
	       width, height, pixel[0], pixel[1], pixel[2], glGetError());
	snprintf(path, sizeof(path), "%s/%s.ready", directory, names[phase]);
	f = fopen(path, "w");
	if (!f)
		fail("open ready marker");
	fprintf(f, "{\"pid\":%d,\"context\":\"%p\",\"monotonic\":%.9f}\n",
		getpid(), SDL_GL_GetCurrentContext(), stamp.tv_sec + stamp.tv_nsec / 1e9);
	if (fclose(f))
		fail("close ready marker");
}

int main(int argc, char **argv)
{
	SDL_Window *window;
	SDL_GLContext context;
	GLuint program, vertex, fragment, vbo;
	GLint ok;
	int previous = -1;
	const GLfloat vertices[] = {-.55f, -.45f, .55f, -.45f, 0, .32f};

	setbuf(stdout, NULL);
	if (argc != 3) {
		fprintf(stderr, "usage: %s PHASE_FILE OUTPUT_DIRECTORY\n", argv[0]);
		return 2;
	}
	if (SDL_Init(SDL_INIT_VIDEO))
		fail("SDL_Init");
	SDL_GL_SetAttribute(SDL_GL_CONTEXT_PROFILE_MASK, SDL_GL_CONTEXT_PROFILE_ES);
	SDL_GL_SetAttribute(SDL_GL_CONTEXT_MAJOR_VERSION, 2);
	SDL_GL_SetAttribute(SDL_GL_CONTEXT_MINOR_VERSION, 0);
	SDL_GL_SetAttribute(SDL_GL_DOUBLEBUFFER, 1);
	SDL_SetHint(SDL_HINT_APP_NAME, "Neo sleep redraw");
	window = SDL_CreateWindow("Neo sleep redraw", SDL_WINDOWPOS_CENTERED,
		SDL_WINDOWPOS_CENTERED, 1200, 750, SDL_WINDOW_OPENGL | SDL_WINDOW_FULLSCREEN_DESKTOP);
	if (!window)
		fail("create window");
	context = SDL_GL_CreateContext(window);
	if (!context)
		fail("create context");
	/* Complete the initial Wayland map/configure before the measured frame. */
	glClearColor(0, 0, 0, 1);
	glClear(GL_COLOR_BUFFER_BIT);
	SDL_GL_SwapWindow(window);
	SDL_Delay(100);
	SDL_PumpEvents();
	printf("vendor=%s renderer=%s version=%s\n", glGetString(GL_VENDOR),
	       glGetString(GL_RENDERER), glGetString(GL_VERSION));
	vertex = shader(GL_VERTEX_SHADER,
		"attribute vec2 position; void main(){gl_Position=vec4(position,0.0,1.0);}");
	fragment = shader(GL_FRAGMENT_SHADER,
		"precision mediump float; uniform vec3 color; void main(){gl_FragColor=vec4(color,1.0);}");
	program = glCreateProgram();
	glAttachShader(program, vertex);
	glAttachShader(program, fragment);
	glBindAttribLocation(program, 0, "position");
	glLinkProgram(program);
	glGetProgramiv(program, GL_LINK_STATUS, &ok);
	if (!ok)
		fail("link program");
	glGenBuffers(1, &vbo);
	glBindBuffer(GL_ARRAY_BUFFER, vbo);
	glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW);
	glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, NULL);
	glEnableVertexAttribArray(0);
	for (;;) {
		SDL_Event event;
		int phase = previous;
		FILE *f;

		while (SDL_PollEvent(&event)) {
			if (event.type == SDL_QUIT || (event.type == SDL_KEYDOWN &&
			    event.key.keysym.sym == SDLK_ESCAPE))
				goto out;
		}
		f = fopen(argv[1], "r");
		if (f) {
			if (fscanf(f, "%d", &phase) != 1)
				phase = previous;
			fclose(f);
		}
		if (phase == 3)
			break;
		if (phase >= 0 && phase <= 4 && phase != previous) {
			draw(window, program, phase, argv[2]);
			previous = phase;
		}
		SDL_Delay(50);
	}
out:
	glDeleteBuffers(1, &vbo);
	glDeleteProgram(program);
	glDeleteShader(vertex);
	glDeleteShader(fragment);
	SDL_GL_DeleteContext(context);
	SDL_DestroyWindow(window);
	SDL_Quit();
	return 0;
}
