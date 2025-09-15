import HomepageReturning from "@/components/HomepageReturning";
import HomepageNewUser from "../components/HomepageNewUser";

interface HomepageProps {
  isReturning: boolean;
}

const Homepage = ({ isReturning }: HomepageProps) => {
  if (isReturning) {
    return <HomepageReturning />;
  } else {
    return <HomepageNewUser />
  }
};

export default Homepage;
