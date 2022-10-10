package main

import (
	"fmt"
	"net/http"
	"os"
    "sync"

	"github.com/gin-gonic/gin"
)



func main() {
	fmt.Println(os.Environ())
	gin.SetMode(gin.ReleaseMode)
	r := gin.Default()
    server := &http.Server{Addr: ":80", Handler: r}

    done := make(map[string]bool)
    done["GET"] = false
    done["POST"] = false
    done["PUT"] = false
    done["DELETE"] = false
    get_handler := func (method string) func(c *gin.Context) {
        return func(c *gin.Context) {
            fmt.Printf("%s: Request completed\n", method)
            c.String(http.StatusOK, method)
            defer func() {
                done[method] = true;
                for _, isDone := range done {
                    if !isDone {
                        return
                    }
                }
                fmt.Printf("All done after %s.\n", method)
                fmt.Println("done map:", done)
                server.Shutdown(c)
            }()
        }
    }

	r.GET("/", get_handler("GET"))
	r.POST("/", get_handler("POST"))
	r.PUT("/", get_handler("PUT"))
	r.DELETE("/", get_handler("DELETE"))

	fmt.Println("Server listening on port 80")

    serverDone := &sync.WaitGroup{}
    serverDone.Add(1)
    go func() {
        defer serverDone.Done()
        if err := server.ListenAndServe(); err != http.ErrServerClosed {
            fmt.Println("Error: Go app can't start listening.")
            os.Exit(1)
        }
    }()
    serverDone.Wait()
}
