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
	var done [4]bool;

	r.GET("/", func(c *gin.Context) {
		fmt.Println("GET: Request completed")
		c.String(http.StatusOK, "GET")
		defer func() {
            done[0] = true;
            if done[1] && done[2] && done[3] {
                os.Exit(0)
            }
		}()
	})

	r.POST("/", func(c *gin.Context) {
		_, err := c.GetRawData()
		if err != nil {
			fmt.Printf("POST: Error getting raw data: %v\n", err)
		}
		fmt.Println("POST: Request completed")
		c.String(http.StatusOK, "POST")
		defer func() {
            done[1] = true;
            if done[0] && done[2] && done[3] {
                os.Exit(0)
            }
		}()
	})

	r.PUT("/", func(c *gin.Context) {
		fmt.Println("PUT: Request completed")
		c.String(http.StatusOK, "PUT")
		defer func() {
            done[2] = true;
            if done[0] && done[1] && done[3] {
                os.Exit(0)
            }
		}()
	})

	r.DELETE("/", func(c *gin.Context) {
		fmt.Println("DELETE: Request completed")
		c.String(http.StatusOK, "DELETE")
		defer func() {
            done[3] = true;
            if done[0] && done[1] && done[2] {
                os.Exit(0)
            }
		}()
	})

	fmt.Println("Server listening on port 80")
	r.Run(":80")
}
