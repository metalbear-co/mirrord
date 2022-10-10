package main

import (
	"fmt"
	"net/http"
	"os"

	"github.com/gin-gonic/gin"
)



func main() {
	fmt.Println(os.Environ())
	gin.SetMode(gin.ReleaseMode)
	r := gin.Default()
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
                os.Exit(0)
            }()
        }
    }

	r.GET("/", get_handler("GET"))
	r.POST("/", get_handler("POST"))
	r.PUT("/", get_handler("PUT"))
	r.DELETE("/", get_handler("DELETE"))

	fmt.Println("Server listening on port 80")
	r.Run(":80")
}
