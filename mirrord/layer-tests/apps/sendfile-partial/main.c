#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <unistd.h>

#ifdef __linux__
#include <sys/sendfile.h>
#endif

/// Writes a source file twice the size of a pipe, then `sendfile`s it into a non-blocking pipe
/// that has room for less than what is asked:
///
/// 1. With an offset, into a pipe with one free page. Checks that one page is sent, `*offset`
///    moves by one page, and the file position stays put.
/// 2. Without an offset, into a full pipe. Checks for -1 with `EAGAIN` and that the file position
///    stays put.
/// 3. Without an offset, into a pipe with one free page. Checks that one page is sent and the file
///    position moves by one page.
/// 4. The whole file, with and without an offset, reading the pipe empty after every call. Checks
///    that what comes out of the pipe matches the file byte for byte.
///
/// The pipe is filled one page at a time, so freeing one page leaves exactly one page of room.
/// Only Linux accepts a pipe as the `sendfile` output, so elsewhere the app does nothing.
static const char *SOURCE_PATH = "/tmp/sendfile_partial_source";

#ifdef __linux__

static size_t page;
static size_t source_size;
static unsigned char *source_data;

static void fill_pipe(int pipe_write, unsigned char *chunk)
{
  while (write(pipe_write, chunk, page) > 0)
  {
  }
  assert(errno == EAGAIN);
}

static void drain_pipe(int pipe_read, unsigned char *chunk)
{
  while (read(pipe_read, chunk, page) > 0)
  {
  }
  assert(errno == EAGAIN);
}

/// Leaves the pipe with exactly one free page.
static void fill_pipe_but_one_page(int pipe_read, int pipe_write, unsigned char *chunk)
{
  drain_pipe(pipe_read, chunk);
  fill_pipe(pipe_write, chunk);
  assert(read(pipe_read, chunk, page) == (ssize_t)page);
}

static void partial_write_with_offset(int source, int pipe_read, int pipe_write, unsigned char *chunk)
{
  fill_pipe_but_one_page(pipe_read, pipe_write, chunk);
  assert(lseek(source, 0, SEEK_SET) == 0);

  off_t offset = 0;
  ssize_t result = sendfile(pipe_write, source, &offset, source_size);
  off_t position = lseek(source, 0, SEEK_CUR);
  if (result != (ssize_t)page || offset != (off_t)page || position != 0)
  {
    fprintf(stderr, "partial write with offset: sendfile returned %zd, offset %lld, position %lld\n",
            result, (long long)offset, (long long)position);
  }
  assert(result == (ssize_t)page);
  assert(offset == (off_t)page);
  assert(position == 0);
}

static void eagain_without_offset(int source, int pipe_read, int pipe_write, unsigned char *chunk)
{
  drain_pipe(pipe_read, chunk);
  fill_pipe(pipe_write, chunk);
  assert(lseek(source, 0, SEEK_SET) == 0);

  ssize_t result = sendfile(pipe_write, source, NULL, page);
  int sendfile_errno = errno;
  off_t position = lseek(source, 0, SEEK_CUR);
  if (result != -1 || sendfile_errno != EAGAIN || position != 0)
  {
    fprintf(stderr, "EAGAIN without offset: sendfile returned %zd, errno %d, position %lld\n",
            result, sendfile_errno, (long long)position);
  }
  assert(result == -1);
  assert(sendfile_errno == EAGAIN);
  assert(position == 0);
}

static void partial_write_without_offset(int source, int pipe_read, int pipe_write, unsigned char *chunk)
{
  fill_pipe_but_one_page(pipe_read, pipe_write, chunk);
  assert(lseek(source, 0, SEEK_SET) == 0);

  ssize_t result = sendfile(pipe_write, source, NULL, source_size);
  off_t position = lseek(source, 0, SEEK_CUR);
  if (result != (ssize_t)page || position != (off_t)page)
  {
    fprintf(stderr, "partial write without offset: sendfile returned %zd, position %lld\n",
            result, (long long)position);
  }
  assert(result == (ssize_t)page);
  assert(position == (off_t)page);
}

/// Sends the whole source through the pipe, always asking for everything that is left, which is
/// more than the pipe can hold. Whatever goes through must match the source byte for byte.
static void copy_whole_file(int source, int pipe_read, int pipe_write, unsigned char *chunk, int use_offset)
{
  drain_pipe(pipe_read, chunk);
  assert(lseek(source, 0, SEEK_SET) == 0);

  unsigned char *received = malloc(source_size);
  assert(received != NULL);
  size_t sent = 0;
  size_t received_size = 0;
  off_t offset = 0;

  while (sent < source_size)
  {
    ssize_t result = sendfile(pipe_write, source, use_offset ? &offset : NULL, source_size - sent);
    if (result == 0)
    {
      break;
    }
    if (result < 0)
    {
      assert(errno == EAGAIN);
    }
    else
    {
      sent += result;
    }

    ssize_t bytes;
    while ((bytes = read(pipe_read, received + received_size, source_size - received_size)) > 0)
    {
      received_size += bytes;
    }
  }

  if (sent != source_size || received_size != source_size || memcmp(received, source_data, source_size) != 0)
  {
    fprintf(stderr, "copy %s offset: sent %zu, received %zu, expected %zu, contents %s\n",
            use_offset ? "with" : "without", sent, received_size, source_size,
            received_size == source_size && memcmp(received, source_data, source_size) == 0 ? "match" : "differ");
  }
  assert(sent == source_size);
  assert(received_size == source_size);
  assert(memcmp(received, source_data, source_size) == 0);

  free(received);
}

int main()
{
  page = (size_t)sysconf(_SC_PAGESIZE);

  int pipes[2];
  assert(pipe2(pipes, O_NONBLOCK) == 0);
  int pipe_read = pipes[0];
  int pipe_write = pipes[1];

  // Bigger than the pipe, so a single `sendfile` of the whole source can never fit.
  int pipe_size = fcntl(pipe_write, F_GETPIPE_SZ);
  assert(pipe_size > 0);
  source_size = (size_t)pipe_size * 2;

  // A pattern that doesn't repeat on page boundaries, so skipped bytes show up as a mismatch.
  source_data = malloc(source_size);
  assert(source_data != NULL);
  for (size_t i = 0; i < source_size; i++)
  {
    source_data[i] = (unsigned char)(i % 251);
  }

  int source = open(SOURCE_PATH, O_CREAT | O_RDWR | O_TRUNC, 0600);
  assert(source >= 0);
  assert(write(source, source_data, source_size) == (ssize_t)source_size);
  assert(unlink(SOURCE_PATH) == 0);

  unsigned char *chunk = malloc(page);
  assert(chunk != NULL);
  memset(chunk, 'x', page);

  partial_write_with_offset(source, pipe_read, pipe_write, chunk);
  eagain_without_offset(source, pipe_read, pipe_write, chunk);
  partial_write_without_offset(source, pipe_read, pipe_write, chunk);
  copy_whole_file(source, pipe_read, pipe_write, chunk, 1);
  copy_whole_file(source, pipe_read, pipe_write, chunk, 0);

  free(chunk);
  free(source_data);
  return 0;
}

#else

int main()
{
  (void)SOURCE_PATH;
  return 0;
}

#endif
