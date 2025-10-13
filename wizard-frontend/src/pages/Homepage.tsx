import HomepageReturning from "@/components/HomepageReturning";
import HomepageNewUser from "../components/HomepageNewUser";
import { useContext } from "react";
import { ConfigDataContextProvider, UserDataContext } from "@/components/UserDataContext";

const Homepage = () => {
  const isReturning = useContext(UserDataContext);
  return (<ConfigDataContextProvider>
    {isReturning ? <HomepageReturning /> : <HomepageNewUser />}
  </ConfigDataContextProvider>);
};

export default Homepage;
