package main

import "C"

import (
	"context"
	"fmt"
	"net"
	"os"
	"os/signal"
	"sync"
	"syscall"
	"time"
	"io"
)

var (
	listenerMu sync.Mutex
	listener   net.Listener
	cancel     context.CancelFunc
	running    bool
)

//export StartServer
func StartServer(chost *C.char, cport C.int) C.int {
	host := C.GoString(chost)
	port := int(cport)

	listenerMu.Lock()
	defer listenerMu.Unlock()

	if running {
		fmt.Println("server already running")
		return 1
	}

	addr := fmt.Sprintf("%s:%d", host, port)
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		fmt.Printf("failed to listen on %s: %v\n", addr, err)
		return -1
	}

	ctx, cancelFunc := context.WithCancel(context.Background())
	cancel = cancelFunc
	listener = ln
	running = true

	go acceptLoop(ctx, ln)

	fmt.Printf("Go server listening on %s\n", addr)
	return 0
}

func acceptLoop(ctx context.Context, ln net.Listener) {
	defer func() {
		listenerMu.Lock()
		listener = nil
		cancel = nil
		running = false
		listenerMu.Unlock()
	}()

	var wg sync.WaitGroup
	connCh := make(chan net.Conn)
	errCh := make(chan error, 1)

	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				errCh <- err
				return
			}
			connCh <- conn
		}
	}()

	for {
		select {
		case <-ctx.Done():
			ln.Close()
			wg.Wait()
			return
		case err := <-errCh:
			if ne, ok := err.(net.Error); ok && ne.Temporary() {
				fmt.Printf("temporary accept error: %v\n", err)
				time.Sleep(100 * time.Millisecond)
				continue
			}
			fmt.Printf("accept error: %v\n", err)
			wg.Wait()
			return
		case conn := <-connCh:
			wg.Add(1)
			go func(c net.Conn) {
				defer wg.Done()
				handleConnection(c)
			}(conn)
		}
	}
}

func handleConnection(conn net.Conn) {
	remoteAddr := conn.RemoteAddr().String()
	fmt.Printf("new client connection from %s\n", remoteAddr)
	defer func() {
		conn.Close()
		fmt.Printf("connection from %s closed\n", remoteAddr)
	}()

	buf := make([]byte, 4096)
	for {
		n, err := conn.Read(buf)
		if err != nil {
			if err != io.EOF {
				fmt.Printf("Connection %s error: %v\n", remoteAddr, err)
			}
			return
		}
		fmt.Printf("connection data from %s: %q\n", remoteAddr, string(buf[:n]))
		_, werr := conn.Write(buf[:n])
		if werr != nil {
			fmt.Printf("write error to %s: %v\n", remoteAddr, werr)
			return
		}
	}
}

//export StopServer
func StopServer() {
	listenerMu.Lock()
	defer listenerMu.Unlock()
	if !running {
		fmt.Println("server not running")
		return
	}
	if cancel != nil {
		cancel()
	}
	if listener != nil {
		_ = listener.Close()
	}
	time.Sleep(100 * time.Millisecond)
	fmt.Println("server stopped")
}

func main() {
	fmt.Println("shared library main() (only when executed directly)")
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
	<-sig
	fmt.Println("exiting main")
}
