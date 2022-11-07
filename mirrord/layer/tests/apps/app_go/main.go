package main

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"sync"
	"time"

	"github.com/gin-gonic/gin"
)

func main() {
	fmt.Println(os.Environ())
	gin.SetMode(gin.ReleaseMode)
	r := gin.Default()
	server := &http.Server{Addr: ":80", Handler: r}

	var wg sync.WaitGroup
	var mut_done sync.Mutex
	done := make(map[string]bool)
	done["GET"] = false
	done["POST"] = false
	done["PUT"] = false
	done["DELETE"] = false

	wg.Add(len(done))

	get_handler := func(method string) func(c *gin.Context) {
		return func(c *gin.Context) {
			fmt.Printf("%s: Request completed\n", method)
			c.String(http.StatusOK, method)

			mut_done.Lock()
			first := !done[method] // Is this the first request of this method.
			done[method] = true
			mut_done.Unlock()

			if first { // So that methods don't get counted more than once if they're sent multiple times.
				defer wg.Done() // Tell main we're done.
			}
		}
	}

	r.GET("/", get_handler("GET"))
	r.POST("/", get_handler("POST"))
	r.PUT("/", get_handler("PUT"))
	r.DELETE("/", get_handler("DELETE"))

	go func() {
		fmt.Println("Server listening on port 80")
		if err := server.ListenAndServe(); err != nil && err != http.ErrServerClosed {
			fmt.Println("Error: listen: %s\n", err)
			os.Exit(1)
		}
	}()

	fmt.Println("Main: waiting for all unique requests to be done.")
	wg.Wait() // Wait for all methods to be done for the first time before shutting down server.
	fmt.Println("Main: all done, shutting server down.")

	// Shutdown server gracefully. If graceful shutdown is not done in 2 seconds just cancel.
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	if err := server.Shutdown(ctx); err != nil {
		fmt.Println("Error: Server Shutdown:", err)
		os.Exit(1)
	}
	// catching ctx.Done(). timeout of 2 seconds.
	select {
	case <-ctx.Done():
		fmt.Println("Server exiting")
	}
}
